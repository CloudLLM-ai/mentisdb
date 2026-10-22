//! Static checks over the embedded dashboard HTML/JS.

const INDEX_HTML: &str = include_str!("../src/dashboard_static/index.html");

#[test]
fn dashboard_has_no_inline_event_handlers_with_interpolation() {
    for attr in [
        "onclick=",
        "onchange=",
        "oninput=",
        "onsubmit=",
        "onkeydown=",
    ] {
        for (i, line) in INDEX_HTML.lines().enumerate() {
            if let Some(pos) = line.find(attr) {
                assert!(
                    !line[pos..].contains("${"),
                    "line {}: inline {attr} handler interpolates data (XSS risk): {}",
                    i + 1,
                    line.trim()
                );
            }
        }
    }
}

#[test]
fn dashboard_api_helper_always_sends_csrf_header() {
    assert!(INDEX_HTML.contains("'X-MentisDB-Dashboard': '1'"));
    assert_eq!(
        INDEX_HTML.matches("fetch(").count(),
        1,
        "all requests must go through the api() helper"
    );
}
