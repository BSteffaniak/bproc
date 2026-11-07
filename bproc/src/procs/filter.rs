//! Search / filter language.
//!
//! Plain words match the name or command line (substring first, then an
//! fzf-style fuzzy match on the name). Structured terms use column filter
//! keys: `cpu>10`, `mem>=2.5`, `rss>500M`, `age>1h`, `time>30s`,
//! `user:root`, `user:me`, `state:Z`, `port:443`, `pid:1234`, `name:node`,
//! `is:ghost`. A leading `-` negates a term (`-user:root`); `a | b` ORs
//! adjacent terms, and space-separated groups are ANDed. Double quotes make
//! phrases (`"foo bar"`, `cmd:"--port 80"`). Smart case: a term containing an
//! uppercase letter is case-sensitive, otherwise matching ignores case.
//!
//! Operators: `:` (contains for text, equals for numbers), `=`/`==`
//! (equals), `!=`, `>`, `>=`, `<`, `<=`. Byte columns take `K/M/G/T`
//! (1024-based), `age`/`time` take `ms/s/m/h/d/w` (compound like `1h30m`;
//! bare numbers are seconds), counts take `k/m/g` (1000-based), `cpu`/`mem`
//! accept a trailing `%`. Terms starting with `--` are plain words so flags
//! like `--port` can be searched unquoted.
use super::columns::{ALL_COLUMNS, ColumnCtx, ColumnId};
use super::model::ProcState;
use super::store::Row;
use std::borrow::Cow;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Filter {
    source: String,
    /// ANDed groups of ORed terms.
    groups: Vec<Vec<Term>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterError {
    pub message: String,
    /// Byte range of the offending input.
    pub span: (usize, usize),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Match {
    /// Char ranges in the displayed Name/Command text to highlight.
    pub highlights: Vec<(usize, usize)>,
}

/// Upper bound on highlight ranges contributed by one term.
const MAX_RANGES_PER_TERM: usize = 32;

#[derive(Debug, Clone, PartialEq)]
struct Term {
    negated: bool,
    kind: Kind,
}

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    /// Plain word (`fuzzy`) or quoted phrase.
    Text {
        needle: Needle,
        fuzzy: bool,
    },
    Field {
        field: TextField,
        op: TextOp,
        needle: Needle,
    },
    /// `user:me`; `None` where the platform has no uid.
    Uid(Option<u32>),
    /// Bit set of `ProcState`s (see `state_bit`).
    State {
        mask: u16,
        negate: bool,
    },
    Number {
        column: ColumnId,
        cmp: Cmp,
        value: f64,
    },
    Port {
        cmp: Cmp,
        value: f64,
    },
    Is(Flag),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextField {
    Name,
    Cmd,
    User,
    Exe,
    Group,
    Tty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextOp {
    Contains,
    Equals,
    NotEquals,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cmp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flag {
    Ghost,
    Kernel,
    Throttled,
    Net,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Percent,
    Bytes,
    /// Seconds-based duration (`age`).
    Seconds,
    /// Millisecond-based duration (`time`); bare numbers are seconds.
    Millis,
    Count,
}

/// A pre-folded search string. Case-insensitive needles are stored lowercase
/// so matching folds only the haystack.
#[derive(Debug, Clone, PartialEq)]
struct Needle {
    text: String,
    case_sensitive: bool,
    ascii: bool,
}

#[inline]
fn fold(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

impl Needle {
    fn new(raw: &str) -> Self {
        let case_sensitive = raw.chars().any(char::is_uppercase);
        let text = if case_sensitive {
            raw.to_string()
        } else {
            raw.chars().map(fold).collect()
        };
        Self {
            ascii: text.is_ascii(),
            text,
            case_sensitive,
        }
    }

    #[inline]
    fn eq_char(&self, needle: char, hay: char) -> bool {
        if self.case_sensitive {
            needle == hay
        } else {
            needle == fold(hay)
        }
    }

    /// Calls `sink(start, end)` with char ranges of up to `max`
    /// non-overlapping occurrences; returns how many were found.
    fn find(&self, hay: &str, max: usize, mut sink: impl FnMut(usize, usize)) -> usize {
        let n = self.text.len();
        if n == 0 || max == 0 {
            return 0;
        }
        let mut found = 0;
        if self.ascii && hay.is_ascii() {
            let (h, p) = (hay.as_bytes(), self.text.as_bytes());
            if p.len() > h.len() {
                return 0;
            }
            let mut i = 0;
            while i + p.len() <= h.len() {
                let hit = if self.case_sensitive {
                    &h[i..i + p.len()] == p
                } else {
                    h[i..i + p.len()]
                        .iter()
                        .zip(p)
                        .all(|(a, b)| a.to_ascii_lowercase() == *b)
                };
                if hit {
                    sink(i, i + p.len());
                    found += 1;
                    if found >= max {
                        break;
                    }
                    i += p.len();
                } else {
                    i += 1;
                }
            }
            return found;
        }
        let h: Vec<char> = hay.chars().collect();
        let p: Vec<char> = self.text.chars().collect();
        let mut i = 0;
        while i + p.len() <= h.len() {
            if p.iter().zip(&h[i..]).all(|(&a, &b)| self.eq_char(a, b)) {
                sink(i, i + p.len());
                found += 1;
                if found >= max {
                    break;
                }
                i += p.len();
            } else {
                i += 1;
            }
        }
        found
    }

    fn contains(&self, hay: &str) -> bool {
        self.text.is_empty() || self.find(hay, 1, |_, _| {}) > 0
    }

    fn equals(&self, hay: &str) -> bool {
        if self.case_sensitive {
            return self.text == hay;
        }
        let mut h = hay.chars();
        self.text
            .chars()
            .all(|p| h.next().is_some_and(|c| p == fold(c)))
            && h.next().is_none()
    }

    /// fzf-style subsequence match. Finds the shortest window ending at the
    /// first complete forward match and reports each matched char index.
    fn fuzzy(&self, hay: &str, mut sink: impl FnMut(usize)) -> bool {
        let mut pat = self.text.chars().peekable();
        let mut end = None;
        for (i, c) in hay.chars().enumerate() {
            match pat.peek() {
                Some(&p) if self.eq_char(p, c) => {
                    pat.next();
                    if pat.peek().is_none() {
                        end = Some(i);
                        break;
                    }
                }
                Some(_) => {}
                None => break,
            }
        }
        let Some(end) = end else {
            return self.text.is_empty();
        };
        // Walk back from `end` to tighten the window start.
        let total = hay.chars().count();
        let mut rpat = self.text.chars().rev().peekable();
        let mut start = end;
        for (i, c) in hay.chars().rev().enumerate().skip(total - 1 - end) {
            let idx = total - 1 - i;
            if let Some(&p) = rpat.peek()
                && self.eq_char(p, c)
            {
                rpat.next();
                start = idx;
                if rpat.peek().is_none() {
                    break;
                }
            }
        }
        let mut pat = self.text.chars().peekable();
        for (i, c) in hay.chars().enumerate().skip(start).take(end + 1 - start) {
            if let Some(&p) = pat.peek()
                && self.eq_char(p, c)
            {
                pat.next();
                sink(i);
            }
        }
        true
    }
}

/// Lazily computed text of the row being matched.
struct RowText<'a> {
    row: &'a Row,
    command: Option<Cow<'a, str>>,
    name_offset: Option<Option<usize>>,
}

impl<'a> RowText<'a> {
    fn new(row: &'a Row) -> Self {
        Self {
            row,
            command: None,
            name_offset: None,
        }
    }

    fn command(&mut self) -> &str {
        let record = &self.row.entry.record;
        self.command
            .get_or_insert_with(|| match record.cmdline.as_slice() {
                [] => Cow::Borrowed(record.name.as_str()),
                [one] => Cow::Borrowed(one.as_str()),
                _ => Cow::Owned(record.cmdline.join(" ")),
            })
    }

    /// Char offset of the process name inside the command line, preferring
    /// the basename of argv[0].
    fn name_offset(&mut self) -> Option<usize> {
        if let Some(off) = self.name_offset {
            return off;
        }
        let record = &self.row.entry.record;
        let name = record.name.as_str();
        let off = match record.cmdline.first() {
            None => Some(0),
            Some(_) if name.is_empty() => None,
            Some(argv0) => argv0
                .rfind(name)
                .map(|b| argv0[..b].chars().count())
                .or_else(|| {
                    let cmd = self.command();
                    cmd.find(name).map(|b| cmd[..b].chars().count())
                }),
        };
        self.name_offset = Some(off);
        off
    }
}

impl Filter {
    pub fn parse(input: &str) -> Result<Filter, FilterError> {
        let tokens = lex(input)?;
        let mut groups: Vec<Vec<Term>> = Vec::new();
        let mut pending_or: Option<(usize, usize)> = None;
        for tok in tokens {
            match tok {
                Tok::Or(span) => {
                    if groups.is_empty() || pending_or.is_some() {
                        return Err(err("'|' needs a term on both sides", span));
                    }
                    pending_or = Some(span);
                }
                Tok::Term(raw) => {
                    let term = parse_term(input, &raw)?;
                    match (pending_or.take(), groups.last_mut()) {
                        (Some(_), Some(group)) => group.push(term),
                        _ => groups.push(vec![term]),
                    }
                }
            }
        }
        if let Some(span) = pending_or {
            return Err(err("'|' needs a term on both sides", span));
        }
        Ok(Filter {
            source: input.to_string(),
            groups,
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    pub fn matches(&self, row: &Row, ctx: &ColumnCtx) -> Option<Match> {
        let mut text = RowText::new(row);
        let mut highlights = Vec::new();
        for group in &self.groups {
            let mut any = false;
            for term in group {
                // Every term of a matching group is evaluated so all of its
                // hits are highlighted.
                let want = !term.negated;
                let hit = eval(&term.kind, &mut text, ctx, want, &mut highlights);
                any |= hit != term.negated;
            }
            if !any {
                return None;
            }
        }
        Some(Match {
            highlights: merge_ranges(highlights),
        })
    }
}

fn merge_ranges(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.retain(|(a, b)| a < b);
    ranges.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (a, b) in ranges {
        match out.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// Pushes single-char positions as merged ranges shifted by `offset`.
fn push_positions(out: &mut Vec<(usize, usize)>, offset: usize, pos: usize) {
    let p = offset.saturating_add(pos);
    match out.last_mut() {
        Some(last) if last.1 == p => last.1 = p + 1,
        _ => out.push((p, p + 1)),
    }
}

fn eval(
    kind: &Kind,
    text: &mut RowText<'_>,
    ctx: &ColumnCtx,
    want: bool,
    out: &mut Vec<(usize, usize)>,
) -> bool {
    let row = text.row;
    let record = &row.entry.record;
    match kind {
        Kind::Text { needle, fuzzy } => {
            let max = if want { MAX_RANGES_PER_TERM } else { 1 };
            if needle.find(text.command(), max, |a, b| {
                if want {
                    out.push((a, b))
                }
            }) > 0
            {
                return true;
            }
            name_term(needle, *fuzzy, text, want, out)
        }
        Kind::Field { field, op, needle } => {
            let hay: Option<Cow<'_, str>> = match field {
                TextField::Name => {
                    return match op {
                        TextOp::Contains => name_term(needle, false, text, want, out),
                        TextOp::Equals => {
                            let hit = needle.equals(&record.name);
                            if hit
                                && want
                                && let Some(off) = text.name_offset()
                            {
                                let len = record.name.chars().count();
                                out.push((off, off + len));
                            }
                            hit
                        }
                        TextOp::NotEquals => !needle.equals(&record.name),
                    };
                }
                TextField::Cmd => {
                    let max = if want { MAX_RANGES_PER_TERM } else { 1 };
                    return match op {
                        TextOp::Contains => {
                            needle.find(text.command(), max, |a, b| {
                                if want {
                                    out.push((a, b))
                                }
                            }) > 0
                        }
                        TextOp::Equals => {
                            let cmd = text.command();
                            let hit = needle.equals(cmd);
                            if hit && want {
                                out.push((0, cmd.chars().count()));
                            }
                            hit
                        }
                        TextOp::NotEquals => !needle.equals(text.command()),
                    };
                }
                TextField::User => {
                    let by_name = record
                        .user
                        .as_deref()
                        .is_some_and(|u| text_op(*op, needle, u));
                    let by_uid = || {
                        record
                            .uid
                            .is_some_and(|uid| text_op(*op, needle, &uid.to_string()))
                    };
                    return match op {
                        TextOp::NotEquals => {
                            record
                                .user
                                .as_deref()
                                .is_none_or(|u| text_op(*op, needle, u))
                                && record
                                    .uid
                                    .is_none_or(|uid| text_op(*op, needle, &uid.to_string()))
                        }
                        _ => by_name || by_uid(),
                    };
                }
                TextField::Exe => record.exe.as_deref().map(Cow::Borrowed),
                TextField::Group => record.group.as_deref().map(Cow::Borrowed),
                TextField::Tty => record.tty.as_deref().map(Cow::Borrowed),
            };
            match hay {
                Some(h) => text_op(*op, needle, &h),
                None => *op == TextOp::NotEquals,
            }
        }
        Kind::Uid(uid) => uid.is_some() && record.uid == *uid,
        Kind::State { mask, negate } => {
            let hit = mask & state_bit(row.entry.sample.state) != 0;
            hit != *negate
        }
        Kind::Number { column, cmp, value } => {
            if *column == ColumnId::Started && record.key.start_time == 0 {
                return false;
            }
            column
                .numeric(row, ctx)
                .is_some_and(|v| compare(*cmp, v, *value, unit_of(*column)))
        }
        Kind::Port { cmp, value } => row.net.as_ref().is_some_and(|n| {
            n.connections.iter().any(|c| {
                compare(*cmp, c.local_port as f64, *value, Unit::Count)
                    || compare(*cmp, c.remote_port as f64, *value, Unit::Count)
            })
        }),
        Kind::Is(flag) => match flag {
            Flag::Ghost => row.is_ghost(),
            Flag::Kernel => record.kernel_thread,
            Flag::Throttled => row.net.as_ref().is_some_and(|n| n.throttle_limit.is_some()),
            Flag::Net => row.net.as_ref().is_some_and(|n| {
                n.download_rate
                    .saturating_add(n.upload_rate)
                    .saturating_add(n.total_download)
                    .saturating_add(n.total_upload)
                    > 0
                    || !n.connections.is_empty()
            }),
        },
    }
}

fn text_op(op: TextOp, needle: &Needle, hay: &str) -> bool {
    match op {
        TextOp::Contains => needle.contains(hay),
        TextOp::Equals => needle.equals(hay),
        TextOp::NotEquals => !needle.equals(hay),
    }
}

/// Matches `needle` against the name (substring, then optionally fuzzy) and
/// maps highlight positions into the command line.
fn name_term(
    needle: &Needle,
    fuzzy: bool,
    text: &mut RowText<'_>,
    want: bool,
    out: &mut Vec<(usize, usize)>,
) -> bool {
    let name = text.row.entry.record.name.as_str();
    let mut local: Vec<(usize, usize)> = Vec::new();
    let max = if want { MAX_RANGES_PER_TERM } else { 1 };
    let mut hit = needle.find(name, max, |a, b| local.push((a, b))) > 0;
    if !hit && fuzzy {
        hit = needle.fuzzy(name, |p| push_positions(&mut local, 0, p));
    }
    if !hit {
        return false;
    }
    if !want {
        return true;
    }
    match text.name_offset() {
        Some(off) => out.extend(local.into_iter().map(|(a, b)| (a + off, b + off))),
        None if fuzzy => {
            let cmd = text.command();
            let mut positions = Vec::new();
            if needle.fuzzy(cmd, |p| push_positions(&mut positions, 0, p)) {
                out.extend(positions);
            }
        }
        None => {
            let cmd = text.command();
            needle.find(cmd, max, |a, b| out.push((a, b)));
        }
    }
    true
}

fn compare(cmp: Cmp, value: f64, target: f64, unit: Unit) -> bool {
    let eps = if unit == Unit::Percent { 0.05 } else { 1e-9 };
    let eq = (value - target).abs() <= eps;
    match cmp {
        Cmp::Eq => eq,
        Cmp::Ne => !eq,
        Cmp::Gt => value > target,
        Cmp::Ge => value >= target || eq,
        Cmp::Lt => value < target,
        Cmp::Le => value <= target || eq,
    }
}

const fn state_bit(state: ProcState) -> u16 {
    1 << match state {
        ProcState::Running => 0,
        ProcState::Sleeping => 1,
        ProcState::DiskSleep => 2,
        ProcState::Stopped => 3,
        ProcState::Tracing => 4,
        ProcState::Zombie => 5,
        ProcState::Idle => 6,
        ProcState::Dead => 7,
        ProcState::Unknown => 8,
    }
}

const STATES: [ProcState; 9] = [
    ProcState::Running,
    ProcState::Sleeping,
    ProcState::DiskSleep,
    ProcState::Stopped,
    ProcState::Tracing,
    ProcState::Zombie,
    ProcState::Idle,
    ProcState::Dead,
    ProcState::Unknown,
];

/// Parses `R`, `ZD`, `t`, `running`, `sleep`, ... into a state bit set.
fn parse_state(value: &str) -> Option<u16> {
    const WORDS: [(&str, ProcState); 11] = [
        ("running", ProcState::Running),
        ("sleeping", ProcState::Sleeping),
        ("disksleep", ProcState::DiskSleep),
        ("disk", ProcState::DiskSleep),
        ("stopped", ProcState::Stopped),
        ("tracing", ProcState::Tracing),
        ("zombie", ProcState::Zombie),
        ("idle", ProcState::Idle),
        ("dead", ProcState::Dead),
        ("unknown", ProcState::Unknown),
        ("defunct", ProcState::Zombie),
    ];
    let lower = value.to_ascii_lowercase();
    if lower.len() >= 2
        && let Some((_, s)) = WORDS.iter().find(|(w, _)| w.starts_with(&lower))
    {
        return Some(state_bit(*s));
    }
    let mut mask = 0u16;
    for c in value.chars() {
        let bits = match c {
            // Lowercase codes are forgiving: `t` means stopped or traced.
            't' => state_bit(ProcState::Stopped) | state_bit(ProcState::Tracing),
            c => {
                let up = c.to_ascii_uppercase();
                STATES
                    .iter()
                    .find(|s| s.code() == up)
                    .map(|s| state_bit(*s))?
            }
        };
        mask |= bits;
    }
    (mask != 0).then_some(mask)
}

const fn unit_of(column: ColumnId) -> Unit {
    use ColumnId::*;
    match column {
        Cpu | Mem => Unit::Percent,
        Rss | Virt | DiskRead | DiskWrite | DiskReadTotal | DiskWriteTotal | NetDown | NetUp
        | NetDownTotal | NetUpTotal | Throttle => Unit::Bytes,
        Started => Unit::Seconds,
        CpuTime => Unit::Millis,
        _ => Unit::Count,
    }
}

const fn example(unit: Unit) -> &'static str {
    match unit {
        Unit::Percent => "10",
        Unit::Bytes => "500M",
        Unit::Seconds => "1h",
        Unit::Millis => "30s",
        Unit::Count => "8",
    }
}

/// Splits a leading decimal number off `s`.
fn split_number(s: &str) -> Option<(f64, &str)> {
    let b = s.as_bytes();
    let mut i = 0;
    if matches!(b.first(), Some(b'-' | b'+')) {
        i = 1;
    }
    let digits_start = i;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
        i += 1;
    }
    if !b[digits_start..i].iter().any(u8::is_ascii_digit) {
        return None;
    }
    s[..i].parse::<f64>().ok().map(|v| (v, &s[i..]))
}

fn parse_value(raw: &str, unit: Unit, span: (usize, usize)) -> Result<f64, FilterError> {
    let bad_number = || err(format!("'{raw}' isn't a number"), span);
    let Some((mut value, rest)) = split_number(raw) else {
        return Err(bad_number());
    };
    let lower = rest.to_ascii_lowercase();
    match unit {
        Unit::Percent => match lower.as_str() {
            "" | "%" => {}
            _ => {
                return Err(err(
                    format!("unknown unit '{rest}' (use a number or %)"),
                    span,
                ));
            }
        },
        Unit::Count => {
            value *= match lower.as_str() {
                "" => 1.0,
                "k" => 1e3,
                "m" => 1e6,
                "g" => 1e9,
                _ => return Err(err(format!("unknown unit '{rest}' (use k, m or g)"), span)),
            }
        }
        Unit::Bytes => {
            let core = lower.strip_suffix("/s").unwrap_or(&lower);
            let core = core
                .strip_suffix("ib")
                .or_else(|| core.strip_suffix('b').filter(|c| !c.is_empty()))
                .unwrap_or(core);
            let pow = match core {
                "" | "b" => 0,
                "k" => 1,
                "m" => 2,
                "g" => 3,
                "t" => 4,
                "p" => 5,
                _ => {
                    return Err(err(
                        format!("unknown unit '{rest}' (use K, M, G or T)"),
                        span,
                    ));
                }
            };
            value *= 1024f64.powi(pow);
        }
        Unit::Seconds | Unit::Millis => {
            let scale = if unit == Unit::Millis { 1000.0 } else { 1.0 };
            let mut total = 0.0;
            let mut cur = value;
            let mut rest = lower.as_str();
            loop {
                let (unit_len, secs) = if rest.starts_with("ms") {
                    (2, 0.001)
                } else {
                    match rest.as_bytes().first() {
                        None => (0, 1.0),
                        Some(b's') => (1, 1.0),
                        Some(b'm') => (1, 60.0),
                        Some(b'h') => (1, 3600.0),
                        Some(b'd') => (1, 86_400.0),
                        Some(b'w') => (1, 604_800.0),
                        _ => {
                            let shown = &raw[raw.len() - rest.len()..];
                            return Err(err(
                                format!("unknown unit '{shown}' (use s, m, h or d)"),
                                span,
                            ));
                        }
                    }
                };
                total += cur * secs;
                rest = &rest[unit_len..];
                if rest.is_empty() {
                    break;
                }
                match split_number(rest) {
                    Some((v, r)) if !rest.starts_with(['-', '+']) => {
                        cur = v;
                        rest = r;
                    }
                    _ => return Err(bad_number()),
                }
            }
            value = total * scale;
        }
    }
    if !value.is_finite() {
        return Err(bad_number());
    }
    Ok(value)
}

fn err(message: impl Into<String>, span: (usize, usize)) -> FilterError {
    FilterError {
        message: message.into(),
        span,
    }
}

#[derive(Debug)]
enum Tok {
    Or((usize, usize)),
    Term(RawTerm),
}

#[derive(Debug)]
struct RawTerm {
    negated: bool,
    /// Whole term including `-`.
    span: (usize, usize),
    /// Term body after `-`.
    body: (usize, usize),
    /// True when the body is a bare quoted phrase.
    phrase: bool,
}

fn lex(input: &str) -> Result<Vec<Tok>, FilterError> {
    let b = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'|' {
            out.push(Tok::Or((i, i + 1)));
            i += 1;
            continue;
        }
        let start = i;
        let mut negated = false;
        if c == b'-' && b.get(i + 1) != Some(&b'-') {
            match b.get(i + 1) {
                None | Some(b' ' | b'\t' | b'\n' | b'\r' | b'|') => {
                    return Err(err("'-' needs a term to exclude", (i, i + 1)));
                }
                _ => {
                    negated = true;
                    i += 1;
                }
            }
        }
        let body_start = i;
        let phrase = b[i] == b'"';
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'|' {
            if b[i] == b'"' {
                let open = i;
                i += 1;
                loop {
                    match b.get(i) {
                        None => return Err(err("missing closing quote", (open, b.len()))),
                        Some(b'\\') if i + 1 < b.len() => i += 2,
                        Some(b'"') => {
                            i += 1;
                            break;
                        }
                        Some(_) => i += 1,
                    }
                }
            } else {
                i += 1;
            }
        }
        out.push(Tok::Term(RawTerm {
            negated,
            span: (start, i),
            body: (body_start, i),
            phrase: phrase && b[i - 1] == b'"' && unquoted_end(&input[body_start..i]),
        }));
    }
    Ok(out)
}

/// True when `s` is exactly one quoted string.
fn unquoted_end(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return i == b.len() - 1,
            _ => i += 1,
        }
    }
    false
}

/// Removes quotes and `\"` / `\\` escapes.
fn unquote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            c => out.push(c),
        }
    }
    out
}

const OPS: [(&str, Op); 8] = [
    (">=", Op::Ge),
    ("<=", Op::Le),
    ("!=", Op::Ne),
    ("==", Op::Eq),
    (":", Op::Colon),
    ("=", Op::Eq),
    (">", Op::Gt),
    ("<", Op::Lt),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Colon,
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

/// Splits `key<op>` off a term body: `(key_len, op, op_len)`.
fn split_key(body: &str) -> Option<(usize, Op, usize)> {
    let b = body.as_bytes();
    if !b.first()?.is_ascii_alphabetic() {
        return None;
    }
    let key_len = b
        .iter()
        .position(|c| !(c.is_ascii_alphanumeric() || *c == b'_'))?;
    let rest = &body[key_len..];
    OPS.iter()
        .find(|(s, _)| rest.starts_with(s))
        .map(|(s, op)| (key_len, *op, s.len()))
}

enum Key {
    Column(ColumnId),
    Port,
    Is,
}

fn resolve_key(key: &str) -> Option<Key> {
    let lower = key.to_ascii_lowercase();
    let canonical = match lower.as_str() {
        "port" => return Some(Key::Port),
        "is" => return Some(Key::Is),
        "command" | "args" => "cmd",
        "comm" | "process" => "name",
        "owner" | "uid" => "user",
        "status" => "state",
        "res" | "resident" => "rss",
        "vsz" | "virtual" => "virt",
        "thr" | "thread" => "threads",
        "prio" | "priority" => "pri",
        "cputime" => "time",
        "conns" | "connections" => "conn",
        "fd" => "fds",
        "memory" => "mem",
        other => other,
    };
    ColumnId::from_filter_key(canonical).map(Key::Column)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

fn unknown_key(key: &str, span: (usize, usize)) -> FilterError {
    let lower = key.to_ascii_lowercase();
    let mut candidates: Vec<(usize, &str)> = ALL_COLUMNS
        .iter()
        .map(|c| c.filter_key())
        .chain(["port", "is"])
        .filter_map(|k| {
            let d = levenshtein(&lower, k);
            let limit = if lower.len() <= 3 { 1 } else { 2 };
            if d <= limit || (lower.len() >= 2 && k.starts_with(&lower)) {
                Some((d, k))
            } else {
                None
            }
        })
        .collect();
    candidates.sort();
    candidates.truncate(3);
    let hint = if candidates.is_empty() {
        "try cpu, mem, user, name, or quote text".to_string()
    } else {
        let names: Vec<&str> = candidates.iter().map(|(_, k)| *k).collect();
        format!("try {}", names.join(", "))
    };
    err(format!("unknown field '{key}' ({hint})"), span)
}

#[cfg(unix)]
fn current_uid() -> Option<u32> {
    // SAFETY: getuid has no preconditions and cannot fail.
    Some(unsafe { libc::getuid() })
}

#[cfg(not(unix))]
fn current_uid() -> Option<u32> {
    None
}

fn parse_term(input: &str, raw: &RawTerm) -> Result<Term, FilterError> {
    let body = &input[raw.body.0..raw.body.1];
    let negated = raw.negated;
    let text_term = |s: &str, fuzzy: bool| -> Result<Term, FilterError> {
        if s.is_empty() {
            return Err(err("empty quotes", raw.span));
        }
        Ok(Term {
            negated,
            kind: Kind::Text {
                needle: Needle::new(s),
                fuzzy,
            },
        })
    };
    if raw.phrase {
        return text_term(&unquote(body), false);
    }
    let Some((key_len, op, op_len)) = split_key(body) else {
        let word = unquote(body);
        let fuzzy = !body.contains('"');
        return text_term(&word, fuzzy);
    };
    let key = &body[..key_len];
    let key_span = (raw.body.0, raw.body.0 + key_len);
    let value_raw = &body[key_len + op_len..];
    let value_span = (raw.body.0 + key_len + op_len, raw.body.1);
    let op_text = &body[key_len..key_len + op_len];
    let Some(resolved) = resolve_key(key) else {
        return Err(unknown_key(key, key_span));
    };
    let value = unquote(value_raw);
    if value.is_empty() {
        let hint = match &resolved {
            Key::Column(c) => match c {
                ColumnId::State => "R".to_string(),
                ColumnId::User => "root".to_string(),
                ColumnId::Name
                | ColumnId::Command
                | ColumnId::Exe
                | ColumnId::Group
                | ColumnId::Tty => "text".to_string(),
                c => example(unit_of(*c)).to_string(),
            },
            Key::Port => "443".to_string(),
            Key::Is => "ghost".to_string(),
        };
        return Err(err(
            format!("'{key}{op_text}' needs a value, e.g. {key}{op_text}{hint}"),
            raw.span,
        ));
    }
    let text_op = |field_name: &str| -> Result<TextOp, FilterError> {
        match op {
            Op::Colon => Ok(TextOp::Contains),
            Op::Eq => Ok(TextOp::Equals),
            Op::Ne => Ok(TextOp::NotEquals),
            _ => Err(err(
                format!("use {field_name}: or {field_name}= (not {op_text})"),
                (key_span.1, value_span.0),
            )),
        }
    };
    let cmp = match op {
        Op::Colon | Op::Eq => Cmp::Eq,
        Op::Ne => Cmp::Ne,
        Op::Gt => Cmp::Gt,
        Op::Ge => Cmp::Ge,
        Op::Lt => Cmp::Lt,
        Op::Le => Cmp::Le,
    };
    let kind = match resolved {
        Key::Port => Kind::Port {
            cmp,
            value: parse_value(&value, Unit::Count, value_span)?,
        },
        Key::Is => {
            if !matches!(op, Op::Colon | Op::Eq) {
                return Err(err(
                    format!("use is: (not is{op_text})"),
                    (key_span.1, value_span.0),
                ));
            }
            let flag = match value.to_ascii_lowercase().as_str() {
                "ghost" | "exited" | "dead" => Flag::Ghost,
                "kernel" | "kthread" => Flag::Kernel,
                "throttled" | "limited" | "throttle" => Flag::Throttled,
                "net" | "network" | "online" => Flag::Net,
                _ => {
                    return Err(err(
                        format!("unknown flag '{value}' (try ghost, kernel, throttled, net)"),
                        value_span,
                    ));
                }
            };
            Kind::Is(flag)
        }
        Key::Column(column) => {
            let field = match column {
                ColumnId::Name => Some(TextField::Name),
                ColumnId::Command => Some(TextField::Cmd),
                ColumnId::User => Some(TextField::User),
                ColumnId::Exe => Some(TextField::Exe),
                ColumnId::Group => Some(TextField::Group),
                ColumnId::Tty => Some(TextField::Tty),
                _ => None,
            };
            let canonical = column.filter_key();
            if let Some(field) = field {
                let op = text_op(canonical)?;
                if field == TextField::User && value.eq_ignore_ascii_case("me") {
                    let kind = Kind::Uid(current_uid());
                    return Ok(Term {
                        negated: negated != (op == TextOp::NotEquals),
                        kind,
                    });
                }
                Kind::Field {
                    field,
                    op,
                    needle: Needle::new(&value),
                }
            } else if column == ColumnId::State {
                let op = text_op(canonical)?;
                let mask = parse_state(&value).ok_or_else(|| {
                    err(
                        format!("unknown state '{value}' (try R, S, D, T, Z or running)"),
                        value_span,
                    )
                })?;
                Kind::State {
                    mask,
                    negate: op == TextOp::NotEquals,
                }
            } else {
                Kind::Number {
                    column,
                    cmp,
                    value: parse_value(&value, unit_of(column), value_span)?,
                }
            }
        }
    };
    Ok(Term { negated, kind })
}
