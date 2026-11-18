//! Terminal output highlighting.
//!
//! Port of upstream `src/terminal/impls/output_highlight.rs` (rule compilation)
//! and the marker matchers from `src/terminal/impls/presentation.rs`. The
//! desktop renderer runs these over each row's plain-text spans; this port does
//! the same in `terminal::rebuild`, so a rule change only needs a repaint.
//!
//! Colours are xterm-256 indices, resolved through the same ANSI table the
//! renderer uses (`terminal::palette_rgb`), which keeps the result identical to
//! an ANSI-coloured program.

use slint::{ModelRc, VecModel};
use std::rc::Rc;

use meatshell_core::config::{ConfigStore, OutputHighlightRule};

use crate::terminal::{palette_rgb, Run};
use crate::OutputRuleItem;

/// Upper bound shared with upstream: each rule is matched against every row.
const MAX_RULES: usize = 128;
const MAX_PATTERN_CHARS: usize = 512;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Preset {
    Off,
    Log,
    DevOps,
}

pub(crate) struct CompiledRule {
    matcher: regex::Regex,
    whole_line: bool,
    ansi_index: u8,
}

/// The compiled form of Settings › Output highlighting, owned by the UI.
pub(crate) struct Rules {
    preset: Preset,
    custom: Vec<CompiledRule>,
}

impl Rules {
    pub(crate) fn off() -> Self {
        Self { preset: Preset::Off, custom: Vec::new() }
    }

    /// The master switch gates the built-in preset and custom rules alike.
    pub(crate) fn enabled(&self) -> bool {
        self.preset != Preset::Off
    }

    /// Read the persisted settings and compile them once for the renderer.
    pub(crate) fn from_store(store: &ConfigStore) -> Self {
        let preset = if !store.output_highlight_enabled() {
            Preset::Off
        } else if store.output_highlight_preset() == "devops" {
            Preset::DevOps
        } else {
            Preset::Log
        };
        let custom = store
            .output_highlight_rules()
            .iter()
            .filter(|rule| rule.enabled && !rule.pattern.trim().is_empty())
            .filter_map(|rule| {
                let pattern = if rule.regex {
                    rule.pattern.clone()
                } else {
                    regex::escape(&rule.pattern)
                };
                let matcher = regex::RegexBuilder::new(&pattern)
                    .case_insensitive(!rule.case_sensitive)
                    .build()
                    .ok()?;
                Some(CompiledRule {
                    matcher,
                    whole_line: rule.whole_line,
                    ansi_index: highlight_color_index(&rule.color),
                })
            })
            .collect();
        Self { preset, custom }
    }
}

fn highlight_color_index(color: &str) -> u8 {
    match color {
        "yellow" => 11,
        "green" => 10,
        "cyan" => 14,
        "magenta" => 13,
        "gray" => 8,
        _ => 9,
    }
}

/// Settings list rows, normalising unknown colours to red like upstream.
pub(crate) fn model(store: &ConfigStore) -> ModelRc<OutputRuleItem> {
    let rows: Vec<OutputRuleItem> = store
        .output_highlight_rules()
        .iter()
        .map(|rule| OutputRuleItem {
            pattern: rule.pattern.clone().into(),
            regex: rule.regex,
            case_sensitive: rule.case_sensitive,
            whole_line: rule.whole_line,
            color: match rule.color.as_str() {
                "yellow" | "green" | "cyan" | "magenta" | "gray" => rule.color.clone(),
                _ => "red".to_string(),
            }
            .into(),
            enabled: rule.enabled,
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(rows)))
}

/// Same checks as upstream `validate_output_highlight_rule`.
pub(crate) fn validate_rule(
    pattern: &str,
    is_regex: bool,
    case_sensitive: bool,
) -> Result<(), String> {
    if pattern.is_empty() {
        return Err("请输入关键词或正则表达式".to_string());
    }
    if pattern.chars().count() > MAX_PATTERN_CHARS {
        return Err("规则不能超过 512 个字符".to_string());
    }
    if is_regex {
        regex::RegexBuilder::new(pattern)
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|error| format!("无效的正则表达式: {error}"))?;
    }
    Ok(())
}

pub(crate) fn at_rule_limit(store: &ConfigStore) -> bool {
    store.output_highlight_rules().len() >= MAX_RULES
}

/// Build the `OutputHighlightRule` the store expects from the dialog fields.
pub(crate) fn rule_from_fields(
    pattern: String,
    regex: bool,
    case_sensitive: bool,
    whole_line: bool,
    color: String,
) -> OutputHighlightRule {
    OutputHighlightRule {
        pattern,
        regex,
        case_sensitive,
        whole_line,
        color,
        enabled: true,
    }
}

/// Highlight one row of plain runs. Runs that already carry styling are left
/// alone, exactly like upstream `custom_rule_eligible`.
pub(crate) fn highlight(mut runs: Vec<Run>, rules: &Rules, default_fg: (u8, u8, u8)) -> Vec<Run> {
    if rules.preset == Preset::Off {
        return runs;
    }
    for rule in &rules.custom {
        runs = apply_custom(runs, rule, default_fg);
    }
    apply_preset(runs, rules.preset, default_fg)
}

fn eligible(run: &Run, default_fg: (u8, u8, u8)) -> bool {
    run.bg.is_none()
        && !run.bold
        && !run.underline
        && run.fg == slint::Color::from_rgb_u8(default_fg.0, default_fg.1, default_fg.2)
}

fn apply_custom(runs: Vec<Run>, rule: &CompiledRule, default_fg: (u8, u8, u8)) -> Vec<Run> {
    if rule.whole_line
        && runs
            .iter()
            .any(|run| eligible(run, default_fg) && rule.matcher.is_match(&run.text))
    {
        let mut out = runs;
        for run in &mut out {
            if eligible(run, default_fg) {
                run.fg = ansi_color(rule.ansi_index);
                run.bold = true;
            }
        }
        return out;
    }

    let mut out = Vec::with_capacity(runs.len() + 2);
    for run in runs {
        if !eligible(&run, default_fg) {
            out.push(run);
            continue;
        }
        let matches: Vec<(usize, usize)> = rule
            .matcher
            .find_iter(&run.text)
            .filter(|m| !m.is_empty())
            .map(|m| (m.start(), m.end()))
            .collect();
        if matches.is_empty() {
            out.push(run);
        } else {
            out.extend(style_matches(run, &matches, rule.ansi_index));
        }
    }
    out
}

fn style_matches(run: Run, matches: &[(usize, usize)], ansi_index: u8) -> Vec<Run> {
    let mut out = Vec::with_capacity(matches.len() * 2 + 1);
    let mut byte_pos = 0usize;
    let mut col = run.col;
    for &(start, end) in matches {
        if start < byte_pos || end > run.text.len() {
            continue;
        }
        if start > byte_pos {
            let text = &run.text[byte_pos..start];
            let cells = cell_width(text);
            let mut part = run.clone();
            part.text = text.to_string();
            part.col = col;
            part.cells = cells;
            out.push(part);
            col += cells;
        }

        let text = &run.text[start..end];
        let cells = cell_width(text);
        let mut hit = run.clone();
        hit.text = text.to_string();
        hit.fg = ansi_color(ansi_index);
        hit.bold = true;
        hit.col = col;
        hit.cells = cells;
        out.push(hit);
        col += cells;
        byte_pos = end;
    }
    if byte_pos < run.text.len() {
        let mut part = run;
        part.text = part.text[byte_pos..].to_string();
        part.col = col;
        part.cells = cell_width(&part.text);
        out.push(part);
    }
    out
}

/// Colour the first log-level (or DevOps state) token of each plain run.
fn apply_preset(runs: Vec<Run>, preset: Preset, default_fg: (u8, u8, u8)) -> Vec<Run> {
    const SEARCH_COLS: i32 = 96;

    let mut out = Vec::with_capacity(runs.len() + 2);
    for run in runs {
        let searchable = run.col < SEARCH_COLS && eligible(&run, default_fg);
        let max_chars = SEARCH_COLS.saturating_sub(run.col) as usize;
        let Some((start, end, ansi_index)) = searchable
            .then(|| marker(&run.text, max_chars, preset))
            .flatten()
        else {
            out.push(run);
            continue;
        };

        let before = run.text[..start].to_string();
        let hit = run.text[start..end].to_string();
        let after = run.text[end..].to_string();
        let before_cells = before.chars().count() as i32;
        let hit_cells = hit.chars().count() as i32;

        if !before.is_empty() {
            let mut part = run.clone();
            part.text = before;
            part.cells = before_cells;
            out.push(part);
        }

        let mut marker_run = run.clone();
        marker_run.text = hit;
        marker_run.fg = ansi_color(ansi_index);
        marker_run.bold = true;
        marker_run.col += before_cells;
        marker_run.cells = hit_cells;
        out.push(marker_run);

        if !after.is_empty() {
            let mut part = run;
            part.text = after;
            part.col += before_cells + hit_cells;
            part.cells = part.cells.saturating_sub(before_cells + hit_cells);
            out.push(part);
        }
    }
    out
}

fn ansi_color(index: u8) -> slint::Color {
    let (r, g, b) = palette_rgb(index, true);
    slint::Color::from_rgb_u8(r, g, b)
}

/// Grid cells for a run slice; mirrors `unicode_width` for the ranges the
/// terminal actually prints (CJK double-width, combining marks zero).
fn cell_width(text: &str) -> i32 {
    text.chars().map(char_cells).sum()
}

fn char_cells(ch: char) -> i32 {
    let cp = ch as u32;
    let wide = (0x1100..=0x115F).contains(&cp)
        || (0x2E80..=0x9FFF).contains(&cp)
        || (0xA960..=0xA97F).contains(&cp)
        || (0xAC00..=0xD7AF).contains(&cp)
        || (0xF900..=0xFAFF).contains(&cp)
        || (0xFE10..=0xFE19).contains(&cp)
        || (0xFE30..=0xFE6F).contains(&cp)
        || (0xFF00..=0xFF60).contains(&cp)
        || (0xFFE0..=0xFFE6).contains(&cp)
        || (0x1F300..=0x1F64F).contains(&cp)
        || (0x1F900..=0x1F9FF).contains(&cp)
        || (0x20000..=0x2FA1F).contains(&cp);
    if wide {
        2
    } else if (0x0300..=0x036F).contains(&cp) || (0xFE00..=0xFE0F).contains(&cp) {
        0
    } else {
        1
    }
}

/// `(byte_start, byte_end, xterm_256_index)` for a log severity marker.
fn log_level_marker(text: &str, max_chars: usize) -> Option<(usize, usize, u8)> {
    const LEVELS: [(&str, u8); 10] = [
        ("CRITICAL", 9),
        ("WARNING", 11),
        ("ERROR", 9),
        ("FATAL", 9),
        ("PANIC", 9),
        ("TRACE", 8),
        ("DEBUG", 8),
        ("NOTICE", 14),
        ("INFO", 14),
        ("WARN", 11),
    ];

    let bytes = text.as_bytes();
    let mut best: Option<(usize, usize, u8)> = None;
    for (word, colour) in LEVELS {
        for (start, _) in text.match_indices(word) {
            if text[..start].chars().count() >= max_chars
                || !ascii_word_boundary(bytes, start, start + word.len())
            {
                continue;
            }
            let candidate = (start, start + word.len(), colour);
            if best.map_or(true, |current| start < current.0) {
                best = Some(candidate);
            }
            break;
        }
    }
    if best.is_some() {
        return best;
    }

    // Structured logs emit `level=error`, `level: warn`, `{"level":"info"}`:
    // accept lowercase values only after a real `level` key.
    let lower = text.to_ascii_lowercase();
    let lower_bytes = lower.as_bytes();
    for (key_start, _) in lower.match_indices("level") {
        if text[..key_start].chars().count() >= max_chars
            || !ascii_word_boundary(lower_bytes, key_start, key_start + 5)
        {
            continue;
        }
        let mut pos = key_start + 5;
        if lower_bytes.get(pos) == Some(&b'"') {
            pos += 1;
        }
        while lower_bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        if !matches!(lower_bytes.get(pos).copied(), Some(b'=') | Some(b':')) {
            continue;
        }
        pos += 1;
        while lower_bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        if matches!(lower_bytes.get(pos).copied(), Some(b'"') | Some(b'\'')) {
            pos += 1;
        }
        for (word, colour) in LEVELS {
            let word = word.to_ascii_lowercase();
            if lower[pos..].starts_with(&word)
                && ascii_word_boundary(lower_bytes, pos, pos + word.len())
            {
                return Some((pos, pos + word.len(), colour));
            }
        }
    }
    None
}

fn marker(text: &str, max_chars: usize, preset: Preset) -> Option<(usize, usize, u8)> {
    let log = log_level_marker(text, max_chars);
    if preset != Preset::DevOps {
        return log;
    }
    let ops = devops_marker(text, max_chars);
    match (log, ops) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(marker), None) | (None, Some(marker)) => Some(marker),
        (None, None) => None,
    }
}

/// Deployment/operations states; ambiguous short words (OK/UP/DOWN) skipped.
fn devops_marker(text: &str, max_chars: usize) -> Option<(usize, usize, u8)> {
    const STATES: [(&str, u8); 15] = [
        ("UNHEALTHY", 9),
        ("SUCCEEDED", 10),
        ("SUCCESS", 10),
        ("FAILURE", 9),
        ("FAILED", 9),
        ("TIMEOUT", 9),
        ("DENIED", 9),
        ("DEGRADED", 11),
        ("RETRYING", 11),
        ("PENDING", 11),
        ("HEALTHY", 10),
        ("READY", 10),
        ("PASSED", 10),
        ("RETRY", 11),
        ("FAIL", 9),
    ];

    let bytes = text.as_bytes();
    let mut best: Option<(usize, usize, u8)> = None;
    for (word, colour) in STATES {
        for (start, _) in text.match_indices(word) {
            if text[..start].chars().count() >= max_chars
                || !ascii_word_boundary(bytes, start, start + word.len())
            {
                continue;
            }
            let candidate = (start, start + word.len(), colour);
            if best.map_or(true, |current| start < current.0) {
                best = Some(candidate);
            }
            break;
        }
    }
    if best.is_some() {
        return best;
    }

    let lower = text.to_ascii_lowercase();
    let lower_bytes = lower.as_bytes();
    for key in ["status", "state", "result"] {
        for (key_start, _) in lower.match_indices(key) {
            if text[..key_start].chars().count() >= max_chars
                || !ascii_word_boundary(lower_bytes, key_start, key_start + key.len())
            {
                continue;
            }
            let mut pos = key_start + key.len();
            if lower_bytes.get(pos) == Some(&b'"') {
                pos += 1;
            }
            while lower_bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
                pos += 1;
            }
            if !matches!(lower_bytes.get(pos).copied(), Some(b'=') | Some(b':')) {
                continue;
            }
            pos += 1;
            while lower_bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
                pos += 1;
            }
            if matches!(lower_bytes.get(pos).copied(), Some(b'"') | Some(b'\'')) {
                pos += 1;
            }
            for (word, colour) in STATES {
                let word = word.to_ascii_lowercase();
                if lower[pos..].starts_with(&word)
                    && ascii_word_boundary(lower_bytes, pos, pos + word.len())
                {
                    return Some((pos, pos + word.len(), colour));
                }
            }
        }
    }
    None
}

fn ascii_word_boundary(bytes: &[u8], start: usize, end: usize) -> bool {
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    bytes
        .get(start.wrapping_sub(1))
        .map_or(true, |b| !is_word(*b))
        && bytes.get(end).map_or(true, |b| !is_word(*b))
}
