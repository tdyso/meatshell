//! Chinese UI text without gettext.
//!
//! The upstream UI source strings are English msgids; `lang/zh/LC_MESSAGES/
//! meatshell.po` holds the Chinese translations. Slint's usual bridge is the
//! `gettext` feature, but OHOS ships `libintl.h` with no `libintl.so`, so the
//! link would fail. Instead i-slint-core is built with its `tr` feature and we
//! install a custom [`tr::Translator`] that serves the same catalogue from the
//! embedded `.po` text. Slint's `@tr(...)` then resolves through it on the
//! device with no extra native dependency.
//!
//! The catalogue is parsed once, lazily, from `include_str!`, so no asset has
//! to be shipped or found at runtime in the HAP sandbox.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::OnceLock;

/// Upstream catalogue, source language English → Chinese.
const ZH_PO: &str = include_str!("../../../lang/zh/LC_MESSAGES/meatshell.po");

struct Catalogue {
    singular: HashMap<String, String>,
}

fn catalogue() -> &'static Catalogue {
    static CATALOGUE: OnceLock<Catalogue> = OnceLock::new();
    CATALOGUE.get_or_init(|| {
        let mut singular = HashMap::new();
        let mut msgid: Option<String> = None;
        let mut msgstr: Option<String> = None;

        fn flush(
            singular: &mut HashMap<String, String>,
            msgid: &mut Option<String>,
            msgstr: &mut Option<String>,
        ) {
            if let (Some(id), Some(text)) = (msgid.take(), msgstr.take()) {
                if !id.is_empty() && !text.is_empty() {
                    singular.insert(id, text);
                }
            }
        }

        for line in ZH_PO.lines() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                flush(&mut singular, &mut msgid, &mut msgstr);
                continue;
            }
            if let Some(rest) = line.strip_prefix("msgid \"") {
                flush(&mut singular, &mut msgid, &mut msgstr);
                msgid = Some(unescape_po(rest.strip_suffix('"').unwrap_or(rest)));
            } else if let Some(rest) = line.strip_prefix("msgstr \"") {
                msgstr = Some(unescape_po(rest.strip_suffix('"').unwrap_or(rest)));
            } else if let Some(rest) = line.strip_prefix('"') {
                let text = unescape_po(rest.strip_suffix('"').unwrap_or(rest));
                // Continuation line belongs to whichever field is being built.
                if msgstr.is_some() {
                    if let Some(msgstr) = msgstr.as_mut() {
                        msgstr.push_str(&text);
                    }
                } else if let Some(msgid) = msgid.as_mut() {
                    msgid.push_str(&text);
                }
            }
        }
        flush(&mut singular, &mut msgid, &mut msgstr);
        Catalogue { singular }
    })
}

fn unescape_po(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => break,
        }
    }
    out
}

struct PoTranslator;

impl tr::Translator for PoTranslator {
    fn translate<'a>(&'a self, msgid: &'a str, _context: Option<&'a str>) -> Cow<'a, str> {
        match catalogue().singular.get(msgid) {
            Some(text) => Cow::Borrowed(text.as_str()),
            None => Cow::Borrowed(msgid),
        }
    }

    fn ntranslate<'a>(
        &'a self,
        n: u64,
        msgid: &'a str,
        msgid_plural: &'a str,
        context: Option<&'a str>,
    ) -> Cow<'a, str> {
        // The upstream catalogue has no plural forms; fall back to English rules.
        if n == 1 {
            self.translate(msgid, context)
        } else {
            Cow::Borrowed(msgid_plural)
        }
    }
}

/// Installs the Chinese catalogue into Slint's global context. Must run after
/// the first component/context exists, so it is called right after the
/// AppWindow is created.
pub(crate) fn install_chinese() {
    // The platform (and therefore the global context) is already installed by
    // the time this runs, so the `factory` closure is never invoked.
    let _ = i_slint_core::with_global_context(
        || unreachable!("Slint context is created by set_platform"),
        |ctx| ctx.set_external_translator(Some(Box::new(PoTranslator))),
    );
}
