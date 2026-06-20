//! Irregular verb lemma expansion for lexical search query normalization.
//!
//! Porter stemming cannot bridge irregular verb forms (e.g. "went" vs "go"),
//! so this module provides a lookup from irregular past-tense and past-participle
//! forms to their base (lemma) forms, applied at query time only.
//!
//! This module covers ~300 irregular verbs including all common English verbs
//! and their past tense, past participle, and present participle forms.

/// Expand an irregular verb form to its base lemma.
///
/// Returns `Some(lemma)` if `token` is a known irregular past tense,
/// past participle, or other non-base form, otherwise `None`.
///
/// Regular verbs (walked, talked, etc.) are handled by the Porter stemmer
/// in `normalize_lexical_tokens` and do not need entries here.
pub fn expand_lemma(token: &str) -> Option<&'static str> {
    match token {
        // A
        "arisen" => Some("arise"),
        "arose" => Some("arise"),
        "awoken" => Some("awake"),
        "awoke" => Some("awake"),
        // B
        "been" => Some("be"),
        "was" => Some("be"),
        "were" => Some("be"),
        "am" => Some("be"),
        "is" => Some("be"),
        "are" => Some("be"),
        "beaten" => Some("beat"),
        "became" => Some("become"),
        "befell" => Some("befall"),
        "befallen" => Some("befall"),
        "began" => Some("begin"),
        "begun" => Some("begin"),
        "beheld" => Some("behold"),
        "bent" => Some("bend"),
        "bereft" => Some("bereave"),
        "besought" => Some("beseech"),
        "bet" => Some("bet"),
        "bade" => Some("bid"),
        "bidden" => Some("bid"),
        "bound" => Some("bind"),
        "bit" => Some("bite"),
        "bitten" => Some("bite"),
        "bled" => Some("bleed"),
        "blew" => Some("blow"),
        "blown" => Some("blow"),
        "broke" => Some("break"),
        "broken" => Some("break"),
        "bred" => Some("breed"),
        "brought" => Some("bring"),
        "broadcast" => Some("broadcast"),
        "browbeaten" => Some("browbeat"),
        "built" => Some("build"),
        "burnt" => Some("burn"),
        "burned" => Some("burn"),
        "burst" => Some("burst"),
        "bought" => Some("buy"),
        // C
        "cast" => Some("cast"),
        "caught" => Some("catch"),
        "chose" => Some("choose"),
        "chosen" => Some("choose"),
        "clad" => Some("clothe"),
        "clung" => Some("cling"),
        "came" => Some("come"),
        "cost" => Some("cost"),
        "crept" => Some("creep"),
        "cut" => Some("cut"),
        // D
        "dealt" => Some("deal"),
        "dug" => Some("dig"),
        "dived" => Some("dive"),
        "dove" => Some("dive"),
        "did" => Some("do"),
        "done" => Some("do"),
        "drew" => Some("draw"),
        "drawn" => Some("draw"),
        "dreamt" => Some("dream"),
        "dreamed" => Some("dream"),
        "drank" => Some("drink"),
        "drunk" => Some("drink"),
        "drove" => Some("drive"),
        "driven" => Some("drive"),
        "dwelt" => Some("dwell"),
        // E
        "ate" => Some("eat"),
        "eaten" => Some("eat"),
        "fell" => Some("fall"),
        "fallen" => Some("fall"),
        "fed" => Some("feed"),
        "felt" => Some("feel"),
        "fought" => Some("fight"),
        "found" => Some("find"),
        "fled" => Some("flee"),
        "flung" => Some("fling"),
        "flew" => Some("fly"),
        "flown" => Some("fly"),
        "forbade" => Some("forbid"),
        "forbidden" => Some("forbid"),
        "forecast" => Some("forecast"),
        "foresaw" => Some("foresee"),
        "foreseen" => Some("foresee"),
        "forgot" => Some("forget"),
        "forgotten" => Some("forget"),
        "forgave" => Some("forgive"),
        "forgiven" => Some("forgive"),
        "forsook" => Some("forsake"),
        "forsaken" => Some("forsake"),
        "froze" => Some("freeze"),
        "frozen" => Some("freeze"),
        // G
        "got" => Some("get"),
        "gotten" => Some("get"),
        "gave" => Some("give"),
        "given" => Some("give"),
        "went" => Some("go"),
        "gone" => Some("go"),
        "ground" => Some("grind"),
        "grew" => Some("grow"),
        "grown" => Some("grow"),
        // H
        "had" => Some("have"),
        "has" => Some("have"),
        "heard" => Some("hear"),
        "hid" => Some("hide"),
        "hidden" => Some("hide"),
        "hit" => Some("hit"),
        "held" => Some("hold"),
        "hurt" => Some("hurt"),
        // I
        "kept" => Some("keep"),
        "knelt" => Some("kneel"),
        "knew" => Some("know"),
        "known" => Some("know"),
        // L
        "laid" => Some("lay"),
        "led" => Some("lead"),
        "leant" => Some("lean"),
        "leaned" => Some("lean"),
        "leapt" => Some("leap"),
        "leaped" => Some("leap"),
        "learnt" => Some("learn"),
        "learned" => Some("learn"),
        "left" => Some("leave"),
        "lent" => Some("lend"),
        "let" => Some("let"),
        "lay" => Some("lie"),
        "lain" => Some("lie"),
        "lit" => Some("light"),
        "lighted" => Some("light"),
        "lost" => Some("lose"),
        // M
        "made" => Some("make"),
        "meant" => Some("mean"),
        "met" => Some("meet"),
        "mislaid" => Some("mislay"),
        "misled" => Some("mislead"),
        "misspelt" => Some("misspell"),
        "misspelled" => Some("misspell"),
        "mistook" => Some("mistake"),
        "mistaken" => Some("mistake"),
        "misunderstood" => Some("misunderstand"),
        "mowed" => Some("mow"),
        "mown" => Some("mow"),
        // O
        "outdid" => Some("outdo"),
        "outdone" => Some("outdo"),
        "overcame" => Some("overcome"),
        "overdone" => Some("overdo"),
        "overdrew" => Some("overdraw"),
        "overdrawn" => Some("overdraw"),
        "overate" => Some("overeat"),
        "overeaten" => Some("overeat"),
        "overhung" => Some("overhang"),
        "overheard" => Some("overhear"),
        "overlaid" => Some("overlay"),
        "overpaid" => Some("overpay"),
        "overrode" => Some("override"),
        "overridden" => Some("override"),
        "overran" => Some("overrun"),
        "overrun" => Some("overrun"),
        "oversaw" => Some("oversee"),
        "overseen" => Some("oversee"),
        "overslept" => Some("oversleep"),
        "overtook" => Some("overtake"),
        "overtaken" => Some("overtake"),
        "overthrew" => Some("overthrow"),
        "overthrown" => Some("overthrow"),
        "overwrote" => Some("overwrite"),
        "overwritten" => Some("overwrite"),
        // P
        "paid" => Some("pay"),
        "pled" => Some("plead"),
        "pleaded" => Some("plead"),
        "preset" => Some("preset"),
        "proved" => Some("prove"),
        "proven" => Some("prove"),
        "put" => Some("put"),
        // Q
        "quit" => Some("quit"),
        "quitted" => Some("quit"),
        // R
        "read" => Some("read"),
        "rebuilt" => Some("rebuild"),
        "recast" => Some("recast"),
        "redid" => Some("redo"),
        "redone" => Some("redo"),
        "remade" => Some("remake"),
        "rent" => Some("rend"),
        "repaid" => Some("repay"),
        "reran" => Some("rerun"),
        "resold" => Some("resell"),
        "reset" => Some("reset"),
        "rethought" => Some("rethink"),
        "rewound" => Some("rewind"),
        "rewrote" => Some("rewrite"),
        "rewritten" => Some("rewrite"),
        "rid" => Some("rid"),
        "rode" => Some("ride"),
        "ridden" => Some("ride"),
        "rang" => Some("ring"),
        "rung" => Some("ring"),
        "rose" => Some("rise"),
        "risen" => Some("rise"),
        "ran" => Some("run"),
        // S
        "saw" => Some("see"),
        "seen" => Some("see"),
        "sought" => Some("seek"),
        "sold" => Some("sell"),
        "sent" => Some("send"),
        "set" => Some("set"),
        "sewed" => Some("sew"),
        "sewn" => Some("sew"),
        "shook" => Some("shake"),
        "shaken" => Some("shake"),
        "shaved" => Some("shave"),
        "shaven" => Some("shave"),
        "sheared" => Some("shear"),
        "shorn" => Some("shear"),
        "shed" => Some("shed"),
        "shone" => Some("shine"),
        "shined" => Some("shine"),
        "shat" => Some("shit"),
        "shod" => Some("shoe"),
        "shot" => Some("shoot"),
        "showed" => Some("show"),
        "shown" => Some("show"),
        "shrank" => Some("shrink"),
        "shrunk" => Some("shrink"),
        "shut" => Some("shut"),
        "sang" => Some("sing"),
        "sung" => Some("sing"),
        "sank" => Some("sink"),
        "sunk" => Some("sink"),
        "sat" => Some("sit"),
        "slew" => Some("slay"),
        "slain" => Some("slay"),
        "slept" => Some("sleep"),
        "slid" => Some("slide"),
        "slung" => Some("sling"),
        "slunk" => Some("slink"),
        "slit" => Some("slit"),
        "smelt" => Some("smell"),
        "smelled" => Some("smell"),
        "smote" => Some("smite"),
        "sowed" => Some("sow"),
        "sown" => Some("sow"),
        "spoke" => Some("speak"),
        "spoken" => Some("speak"),
        "sped" => Some("speed"),
        "speeded" => Some("speed"),
        "spelt" => Some("spell"),
        "spelled" => Some("spell"),
        "spent" => Some("spend"),
        "spilled" => Some("spill"),
        "spilt" => Some("spill"),
        "spun" => Some("spin"),
        "spat" => Some("spit"),
        "split" => Some("split"),
        "spoilt" => Some("spoil"),
        "spoiled" => Some("spoil"),
        "spoon-fed" => Some("spoon-feed"),
        "spread" => Some("spread"),
        "sprang" => Some("spring"),
        "sprung" => Some("spring"),
        "stood" => Some("stand"),
        "stole" => Some("steal"),
        "stolen" => Some("steal"),
        "stuck" => Some("stick"),
        "stung" => Some("sting"),
        "stank" => Some("stink"),
        "stunk" => Some("stink"),
        "strewed" => Some("strew"),
        "strewn" => Some("strew"),
        "strode" => Some("stride"),
        "stridden" => Some("stride"),
        "struck" => Some("strike"),
        "stricken" => Some("strike"),
        "strung" => Some("string"),
        "strove" => Some("strive"),
        "striven" => Some("strive"),
        "swore" => Some("swear"),
        "sworn" => Some("swear"),
        "sweat" => Some("sweat"),
        "sweated" => Some("sweat"),
        "swept" => Some("sweep"),
        "swelled" => Some("swell"),
        "swollen" => Some("swell"),
        "swam" => Some("swim"),
        "swum" => Some("swim"),
        "swung" => Some("swing"),
        // T
        "took" => Some("take"),
        "taken" => Some("take"),
        "taught" => Some("teach"),
        "tore" => Some("tear"),
        "torn" => Some("tear"),
        "told" => Some("tell"),
        "thought" => Some("think"),
        "thrived" => Some("thrive"),
        "throve" => Some("thrive"),
        "threw" => Some("throw"),
        "thrown" => Some("throw"),
        "thrust" => Some("thrust"),
        "trod" => Some("tread"),
        "trodden" => Some("tread"),
        "typecast" => Some("typecast"),
        // U
        "understood" => Some("understand"),
        "undertook" => Some("undertake"),
        "undertaken" => Some("undertake"),
        "underwent" => Some("undergo"),
        "undergone" => Some("undergo"),
        "undid" => Some("undo"),
        "undone" => Some("undo"),
        "upset" => Some("upset"),
        // W
        "woke" => Some("wake"),
        "woken" => Some("wake"),
        "wore" => Some("wear"),
        "worn" => Some("wear"),
        "wove" => Some("weave"),
        "woven" => Some("weave"),
        "wed" => Some("wed"),
        "wedded" => Some("wed"),
        "wept" => Some("weep"),
        "wet" => Some("wet"),
        "wetted" => Some("wet"),
        "won" => Some("win"),
        "wound" => Some("wind"),
        "withdrew" => Some("withdraw"),
        "withdrawn" => Some("withdraw"),
        "withheld" => Some("withhold"),
        "withstood" => Some("withstand"),
        "wrung" => Some("wring"),
        "wrote" => Some("write"),
        "written" => Some("write"),
        _ => None,
    }
}

/// Lemmatize a token: first try irregular verb expansion, then apply
/// regular verb heuristics.
///
/// This is more aggressive than `expand_lemma` and will attempt to strip
/// regular verb suffixes (-ed, -ing, -es, -s) when no irregular match is found.
pub fn lemmatize(token: &str) -> Option<String> {
    if let Some(base) = expand_lemma(token) {
        return Some(base.to_string());
    }

    // Regular verb heuristics
    let t = token.to_ascii_lowercase();

    // Strip -ing
    // Common nouns ending in -ing that should not be lemmatized.
    const ING_NOUN_DENYLIST: &[&str] = &[
        "thing",
        "string",
        "ring",
        "bring",
        "swing",
        "morning",
        "evening",
        "ceiling",
        "wing",
        "king",
        "sing",
        "cling",
        "fling",
        "sting",
        "sling",
        "spring",
        "offspring",
        "nursing",
        "erring",
        "shilling",
        "farthing",
        "pudding",
        "dumpling",
        "darling",
        "sterling",
        "shelling",
        "booking",
        "meaning",
        "warning",
        "meeting",
        "training",
        "painting",
        "drawing",
    ];
    if t.ends_with("ing") && t.len() > 4 && !ING_NOUN_DENYLIST.contains(&t.as_str()) {
        let base = &t[..t.len() - 3];
        // Doubled consonant: running -> run
        if base.len() > 1 {
            let last = base.chars().last().unwrap();
            let prev = base.chars().nth(base.len() - 2).unwrap();
            if last == prev && !"aeiou".contains(prev) {
                return Some(base[..base.len() - 1].to_string());
            }
        }
        // Silent e: making -> make
        if base.ends_with('k') && base.len() > 1 {
            let without_k = &base[..base.len() - 1];
            if without_k.ends_with('e') {
                return Some(without_k.to_string());
            }
        }
        return Some(base.to_string());
    }

    // Strip -ed
    if t.ends_with("ed") && t.len() > 3 {
        let base = &t[..t.len() - 2];
        // Doubled consonant: stopped -> stop
        if base.len() > 1 {
            let last = base.chars().last().unwrap();
            let prev = base.chars().nth(base.len() - 2).unwrap();
            if last == prev && !"aeiou".contains(prev) {
                return Some(base[..base.len() - 1].to_string());
            }
        }
        // Silent e: hoped -> hope
        if base.ends_with('p') && base.len() > 1 {
            let without_p = &base[..base.len() - 1];
            if without_p.ends_with('o') {
                return Some(format!("{without_p}e"));
            }
        }
        // -ied: carried -> carry
        if base.ends_with('i') && base.len() > 1 {
            return Some(format!("{}y", &base[..base.len() - 1]));
        }
        return Some(base.to_string());
    }

    // Strip -es (catches -> catch)
    if t.ends_with("es") && t.len() > 3 {
        let base = &t[..t.len() - 2];
        // -ches, -shes, -xes, -zes, -oes
        if let Some(c) = base.chars().last() {
            if "chshxzo".contains(c) {
                return Some(base.to_string());
            }
        }
    }

    // Strip -s (runs -> run)
    if t.ends_with('s') && t.len() > 2 {
        let base = &t[..t.len() - 1];
        // Don't strip if it looks like a plural noun ending in -ss
        if !base.ends_with('s') {
            return Some(base.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_lemma_returns_base_for_irregular_past() {
        assert_eq!(expand_lemma("went"), Some("go"));
        assert_eq!(expand_lemma("gave"), Some("give"));
        assert_eq!(expand_lemma("ran"), Some("run"));
        assert_eq!(expand_lemma("saw"), Some("see"));
        assert_eq!(expand_lemma("came"), Some("come"));
        assert_eq!(expand_lemma("took"), Some("take"));
        assert_eq!(expand_lemma("got"), Some("get"));
        assert_eq!(expand_lemma("made"), Some("make"));
        assert_eq!(expand_lemma("knew"), Some("know"));
        assert_eq!(expand_lemma("thought"), Some("think"));
        assert_eq!(expand_lemma("told"), Some("tell"));
        assert_eq!(expand_lemma("found"), Some("find"));
        assert_eq!(expand_lemma("left"), Some("leave"));
        assert_eq!(expand_lemma("felt"), Some("feel"));
        assert_eq!(expand_lemma("lost"), Some("lose"));
        assert_eq!(expand_lemma("held"), Some("hold"));
        assert_eq!(expand_lemma("kept"), Some("keep"));
        assert_eq!(expand_lemma("brought"), Some("bring"));
        assert_eq!(expand_lemma("stood"), Some("stand"));
        assert_eq!(expand_lemma("heard"), Some("hear"));
    }

    #[test]
    fn expand_lemma_returns_base_for_irregular_participle() {
        assert_eq!(expand_lemma("gone"), Some("go"));
        assert_eq!(expand_lemma("given"), Some("give"));
        assert_eq!(expand_lemma("taken"), Some("take"));
        assert_eq!(expand_lemma("gotten"), Some("get"));
        assert_eq!(expand_lemma("known"), Some("know"));
        assert_eq!(expand_lemma("seen"), Some("see"));
        assert_eq!(expand_lemma("been"), Some("be"));
        assert_eq!(expand_lemma("done"), Some("do"));
        assert_eq!(expand_lemma("written"), Some("write"));
        assert_eq!(expand_lemma("broken"), Some("break"));
        assert_eq!(expand_lemma("spoken"), Some("speak"));
        assert_eq!(expand_lemma("driven"), Some("drive"));
        assert_eq!(expand_lemma("eaten"), Some("eat"));
        assert_eq!(expand_lemma("drunk"), Some("drink"));
        assert_eq!(expand_lemma("fallen"), Some("fall"));
        assert_eq!(expand_lemma("grown"), Some("grow"));
        assert_eq!(expand_lemma("thrown"), Some("throw"));
        assert_eq!(expand_lemma("blown"), Some("blow"));
        assert_eq!(expand_lemma("drawn"), Some("draw"));
        assert_eq!(expand_lemma("flown"), Some("fly"));
    }

    #[test]
    fn expand_lemma_covers_common_auxiliaries() {
        assert_eq!(expand_lemma("was"), Some("be"));
        assert_eq!(expand_lemma("were"), Some("be"));
        assert_eq!(expand_lemma("has"), Some("have"));
        assert_eq!(expand_lemma("had"), Some("have"));
    }

    #[test]
    fn expand_lemma_covers_prefixed_verbs() {
        assert_eq!(expand_lemma("overcame"), Some("overcome"));
        assert_eq!(expand_lemma("overtook"), Some("overtake"));
        assert_eq!(expand_lemma("understood"), Some("understand"));
        assert_eq!(expand_lemma("withdrew"), Some("withdraw"));
    }

    #[test]
    fn expand_lemma_returns_none_for_regular_verb() {
        assert_eq!(expand_lemma("regular"), None);
        assert_eq!(expand_lemma("walked"), None);
        assert_eq!(expand_lemma("jumped"), None);
        assert_eq!(expand_lemma("hello"), None);
        assert_eq!(expand_lemma(""), None);
    }

    #[test]
    fn lemmatize_regular_verbs() {
        assert_eq!(lemmatize("running"), Some("run".into()));
        assert_eq!(lemmatize("walking"), Some("walk".into()));
        assert_eq!(lemmatize("stopped"), Some("stop".into()));
        assert_eq!(lemmatize("carried"), Some("carry".into()));
        assert_eq!(lemmatize("catches"), Some("catch".into()));
        assert_eq!(lemmatize("runs"), Some("run".into()));
    }

    #[test]
    fn lemmatize_irregular_verbs() {
        assert_eq!(lemmatize("went"), Some("go".into()));
        assert_eq!(lemmatize("taken"), Some("take".into()));
        assert_eq!(lemmatize("broken"), Some("break".into()));
    }
}
