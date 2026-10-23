use ar_reshaper::reshape_line;
use unicode_bidi::{BidiInfo, Level};

/// Check if string contains any Arabic / Persian script characters
pub fn has_rtl(text: &str) -> bool {
    text.chars().any(|c| {
        ('\u{0600}'..='\u{06FF}').contains(&c)
            || ('\u{0750}'..='\u{077F}').contains(&c)
            || ('\u{08A0}'..='\u{08FF}').contains(&c)
            || ('\u{FB50}'..='\u{FDFF}').contains(&c)
            || ('\u{FE70}'..='\u{FEFF}').contains(&c)
    })
}

/// Reshape Arabic/Persian cursive joining and reorder for LTR rendering
pub fn fix_rtl(text: &str) -> String {
    if !has_rtl(text) {
        return text.to_string();
    }
    if text.contains('\n') {
        text.lines().map(fix_rtl_single_line).collect::<Vec<_>>().join("\n")
    } else {
        fix_rtl_single_line(text)
    }
}

fn fix_rtl_single_line(text: &str) -> String {
    if !has_rtl(text) {
        return text.to_string();
    }
    let reshaped = reshape_line(text);
    let bidi_info = BidiInfo::new(&reshaped, Some(Level::ltr()));
    if let Some(para) = bidi_info.paragraphs.first() {
        bidi_info.reorder_line(para, para.range.clone()).into_owned()
    } else {
        reshaped
    }
}
