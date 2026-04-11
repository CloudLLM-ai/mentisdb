//! Irregular verb lemma expansion for lexical search query normalization.
//!
//! Porter stemming cannot bridge irregular verb forms (e.g. "went" vs "go"),
//! so this module provides a lookup from irregular past-tense and past-participle
//! forms to their base (lemma) forms, applied at query time only.

/// Expand an irregular verb form to its base lemma.
///
/// Returns `Some(lemma)` if `token` is a known irregular past tense or
/// past participle form, otherwise `None`.
pub fn expand_lemma(token: &str) -> Option<&'static str> {
    match token {
        "arisen" => Some("arise"),
        "arose" => Some("arise"),
        "awoken" => Some("awake"),
        "awoke" => Some("awake"),
        "beaten" => Some("beat"),
        "became" => Some("become"),
        "been" => Some("be"),
        "befell" => Some("befall"),
        "begun" => Some("begin"),
        "bent" => Some("bend"),
        "bereft" => Some("bereave"),
        "besought" => Some("beseech"),
        "bit" => Some("bite"),
        "bitten" => Some("bite"),
        "bled" => Some("bleed"),
        "blew" => Some("blow"),
        "blown" => Some("blow"),
        "bore" => Some("bear"),
        "born" => Some("bear"),
        "borne" => Some("bear"),
        "bought" => Some("buy"),
        "bound" => Some("bind"),
        "bred" => Some("breed"),
        "broke" => Some("break"),
        "broken" => Some("break"),
        "brought" => Some("bring"),
        "built" => Some("build"),
        "came" => Some("come"),
        "caught" => Some("catch"),
        "chose" => Some("choose"),
        "chosen" => Some("choose"),
        "clung" => Some("cling"),
        "dealt" => Some("deal"),
        "did" => Some("do"),
        "done" => Some("do"),
        "drank" => Some("drink"),
        "drawn" => Some("draw"),
        "drew" => Some("draw"),
        "driven" => Some("drive"),
        "drove" => Some("drive"),
        "drunk" => Some("drink"),
        "dwelt" => Some("dwell"),
        "ate" => Some("eat"),
        "eaten" => Some("eat"),
        "fallen" => Some("fall"),
        "fell" => Some("fall"),
        "fed" => Some("feed"),
        "felt" => Some("feel"),
        "fled" => Some("flee"),
        "flew" => Some("fly"),
        "flown" => Some("fly"),
        "flung" => Some("fling"),
        "found" => Some("find"),
        "forbade" => Some("forbid"),
        "forbidden" => Some("forbid"),
        "forgave" => Some("forgive"),
        "forgiven" => Some("forgive"),
        "forgot" => Some("forget"),
        "forgotten" => Some("forget"),
        "forsaken" => Some("forsake"),
        "forsook" => Some("forsake"),
        "froze" => Some("freeze"),
        "frozen" => Some("freeze"),
        "gave" => Some("give"),
        "given" => Some("give"),
        "gone" => Some("go"),
        "got" => Some("get"),
        "gotten" => Some("get"),
        "grew" => Some("grow"),
        "grown" => Some("grow"),
        "had" => Some("have"),
        "has" => Some("have"),
        "heard" => Some("hear"),
        "held" => Some("hold"),
        "hid" => Some("hide"),
        "hidden" => Some("hide"),
        "kept" => Some("keep"),
        "knew" => Some("know"),
        "knelt" => Some("kneel"),
        "known" => Some("know"),
        "laid" => Some("lay"),
        "lain" => Some("lie"),
        "lay" => Some("lie"),
        "led" => Some("lead"),
        "left" => Some("leave"),
        "lent" => Some("lend"),
        "lost" => Some("lose"),
        "made" => Some("make"),
        "meant" => Some("mean"),
        "met" => Some("meet"),
        "misled" => Some("mislead"),
        "mistaken" => Some("mistake"),
        "mistook" => Some("mistake"),
        "paid" => Some("pay"),
        "pled" => Some("plead"),
        "proven" => Some("prove"),
        "ran" => Some("run"),
        "rang" => Some("ring"),
        "ridden" => Some("ride"),
        "rode" => Some("ride"),
        "risen" => Some("rise"),
        "rose" => Some("rise"),
        "rung" => Some("ring"),
        "said" => Some("say"),
        "sang" => Some("sing"),
        "sat" => Some("sit"),
        "saw" => Some("see"),
        "seen" => Some("see"),
        "shaken" => Some("shake"),
        "shook" => Some("shake"),
        "shone" => Some("shine"),
        "shrank" => Some("shrink"),
        "shrunk" => Some("shrink"),
        "slain" => Some("slay"),
        "slew" => Some("slay"),
        "slid" => Some("slide"),
        "smelt" => Some("smell"),
        "sold" => Some("sell"),
        "sought" => Some("seek"),
        "sown" => Some("sow"),
        "spoke" => Some("speak"),
        "spoken" => Some("speak"),
        "sped" => Some("speed"),
        "spent" => Some("spend"),
        "spun" => Some("spin"),
        "sprang" => Some("spring"),
        "sprung" => Some("spring"),
        "stole" => Some("steal"),
        "stolen" => Some("steal"),
        "stood" => Some("stand"),
        "stove" => Some("stave"),
        "strove" => Some("strive"),
        "stricken" => Some("strike"),
        "striven" => Some("strive"),
        "struck" => Some("strike"),
        "strung" => Some("string"),
        "stuck" => Some("stick"),
        "stung" => Some("sting"),
        "stunk" => Some("stink"),
        "swam" => Some("swim"),
        "sworn" => Some("swear"),
        "swept" => Some("sweep"),
        "swollen" => Some("swell"),
        "swum" => Some("swim"),
        "swung" => Some("swing"),
        "taken" => Some("take"),
        "taught" => Some("teach"),
        "tore" => Some("tear"),
        "torn" => Some("tear"),
        "told" => Some("tell"),
        "took" => Some("take"),
        "thought" => Some("think"),
        "thrown" => Some("throw"),
        "threw" => Some("throw"),
        "understood" => Some("understand"),
        "woken" => Some("wake"),
        "woke" => Some("wake"),
        "were" => Some("be"),
        "wore" => Some("wear"),
        "worn" => Some("wear"),
        "woven" => Some("weave"),
        "wove" => Some("weave"),
        "wept" => Some("weep"),
        "went" => Some("go"),
        "withdrawn" => Some("withdraw"),
        "withdrew" => Some("withdraw"),
        "won" => Some("win"),
        "wound" => Some("wind"),
        "wrung" => Some("wring"),
        "written" => Some("write"),
        "wrote" => Some("write"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::expand_lemma;

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
    fn expand_lemma_returns_none_for_regular_verb() {
        assert_eq!(expand_lemma("regular"), None);
        assert_eq!(expand_lemma("walked"), None);
        assert_eq!(expand_lemma("jumped"), None);
        assert_eq!(expand_lemma("hello"), None);
        assert_eq!(expand_lemma(""), None);
    }
}
