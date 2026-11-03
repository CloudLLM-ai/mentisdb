#![no_main]
//! Fuzzes the prompt-text escaper: whatever a writer stores, what reaches a
//! prompt is one clean line that cannot spell a block delimiter, and escaping
//! is idempotent.

use libfuzzer_sys::fuzz_target;
use mentisdb::prompt_text::{inline_label, inline_text, MAX_LABEL_CHARS};

fn assert_clean(line: &str) {
    assert!(!line.contains("==="), "delimiter survived: {line:?}");
    assert!(!line.contains("[#"), "entry header survived: {line:?}");
    for c in line.chars() {
        assert!(
            !c.is_control() && !matches!(c, '\u{2028}' | '\u{2029}' | '\u{FEFF}' | '\u{202E}'),
            "unsafe char {c:?} survived in {line:?}"
        );
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let line = inline_text(text);
    assert_clean(&line);
    assert_eq!(inline_text(&line), line, "escaping is not idempotent");
    let label = inline_label(text);
    assert_clean(&label);
    assert!(label.chars().count() <= MAX_LABEL_CHARS + 1);
});
