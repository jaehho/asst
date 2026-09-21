//! iCalendar (RFC 5545) content lines, kept faithful to the source.
//!
//! An object parsed and written back unchanged is byte-identical, and setting
//! one property rewrites only that property's lines. This is what lets asst
//! edit a task created on the iPhone without dropping what iOS wrote.

use std::fmt;

/// A whole iCalendar object: usually one VCALENDAR, plus whatever else was
/// in the file (kept verbatim).
#[derive(Debug, Clone, PartialEq)]
pub struct Ical {
    items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Property(Property),
    Component(Component),
    /// A line that is not a valid content line. Kept so output stays faithful.
    Junk(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Component {
    name: String,
    items: Vec<Item>,
    /// The BEGIN line as it was read. `None` for components built here.
    begin_raw: Option<String>,
    /// The END line as it was read; empty when the source never closed it.
    end_raw: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    name: String,
    params: Vec<Param>,
    /// The value as written, still escaped.
    value: String,
    /// The original physical lines. Dropped when the property is changed.
    raw: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    name: String,
    values: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("no VCALENDAR in the object")]
    NoCalendar,
}

impl Ical {
    pub fn parse(input: &str) -> Ical {
        let mut stack: Vec<Component> = Vec::new();
        let mut top: Vec<Item> = Vec::new();

        fn push(stack: &mut [Component], top: &mut Vec<Item>, item: Item) {
            match stack.last_mut() {
                Some(c) => c.items.push(item),
                None => top.push(item),
            }
        }

        for line in logical_lines(input) {
            let Some(prop) = parse_line(&line.unfolded, &line.raw) else {
                push(&mut stack, &mut top, Item::Junk(line.raw));
                continue;
            };
            if prop.is("BEGIN") {
                stack.push(Component {
                    name: prop.value.clone(),
                    items: Vec::new(),
                    begin_raw: Some(line.raw),
                    end_raw: None,
                });
            } else if prop.is("END") {
                let open = stack
                    .iter()
                    .rposition(|c| c.name.eq_ignore_ascii_case(&prop.value));
                match open {
                    Some(at) => {
                        // Close anything left open inside it first.
                        while stack.len() > at + 1 {
                            let mut inner = stack.pop().expect("len checked");
                            inner.end_raw = Some(String::new());
                            push(&mut stack, &mut top, Item::Component(inner));
                        }
                        let mut done = stack.pop().expect("position found");
                        done.end_raw = Some(line.raw);
                        push(&mut stack, &mut top, Item::Component(done));
                    }
                    None => push(&mut stack, &mut top, Item::Junk(line.raw)),
                }
            } else {
                push(&mut stack, &mut top, Item::Property(prop));
            }
        }
        while let Some(mut open) = stack.pop() {
            open.end_raw = Some(String::new());
            push(&mut stack, &mut top, Item::Component(open));
        }
        Ical { items: top }
    }

    pub fn new(calendar: Component) -> Ical {
        Ical {
            items: vec![Item::Component(calendar)],
        }
    }

    pub fn calendar(&self) -> Option<&Component> {
        self.items.iter().find_map(|i| match i {
            Item::Component(c) if c.is("VCALENDAR") => Some(c),
            _ => None,
        })
    }

    pub fn calendar_mut(&mut self) -> Option<&mut Component> {
        self.items.iter_mut().find_map(|i| match i {
            Item::Component(c) if c.is("VCALENDAR") => Some(c),
            _ => None,
        })
    }

    /// The first VTODO. A task resource holds one (plus, in theory,
    /// RECURRENCE-ID overrides, which asst leaves alone).
    pub fn todo(&self) -> Option<&Component> {
        self.calendar()?
            .components()
            .find(|c| c.is("VTODO") && c.property("RECURRENCE-ID").is_none())
    }

    pub fn todo_mut(&mut self) -> Option<&mut Component> {
        self.calendar_mut()?
            .components_mut()
            .find(|c| c.is("VTODO") && c.property("RECURRENCE-ID").is_none())
    }
}

impl fmt::Display for Ical {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::new();
        write_items(&self.items, &mut out);
        f.write_str(&out)
    }
}

impl Component {
    pub fn new(name: &str) -> Component {
        Component {
            name: name.to_string(),
            items: Vec::new(),
            begin_raw: None,
            end_raw: None,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    pub fn properties(&self) -> impl Iterator<Item = &Property> {
        self.items.iter().filter_map(|i| match i {
            Item::Property(p) => Some(p),
            _ => None,
        })
    }

    pub fn property(&self, name: &str) -> Option<&Property> {
        self.properties().find(|p| p.is(name))
    }

    pub fn properties_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Property> {
        self.properties().filter(move |p| p.is(name))
    }

    pub fn components(&self) -> impl Iterator<Item = &Component> {
        self.items.iter().filter_map(|i| match i {
            Item::Component(c) => Some(c),
            _ => None,
        })
    }

    pub fn components_mut(&mut self) -> impl Iterator<Item = &mut Component> {
        self.items.iter_mut().filter_map(|i| match i {
            Item::Component(c) => Some(c),
            _ => None,
        })
    }

    /// Set a single-valued property: replace the first of that name in place
    /// (untouched if equal), drop any others, or add it after the last
    /// property if there was none.
    pub fn set(&mut self, prop: Property) {
        let mut replaced = false;
        self.items.retain_mut(|item| match item {
            Item::Property(p) if p.is(&prop.name) => {
                if replaced {
                    return false;
                }
                replaced = true;
                if !p.same_content(&prop) {
                    *p = prop.clone();
                }
                true
            }
            _ => true,
        });
        if !replaced {
            self.add(prop);
        }
    }

    /// Add a property after the existing ones, before any subcomponent.
    pub fn add(&mut self, prop: Property) {
        let at = self
            .items
            .iter()
            .rposition(|i| matches!(i, Item::Property(_)))
            .map_or(0, |i| i + 1);
        self.items.insert(at, Item::Property(prop));
    }

    /// Remove every property of that name. Returns whether any existed.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.items.len();
        self.items
            .retain(|i| !matches!(i, Item::Property(p) if p.is(name)));
        self.items.len() != before
    }

    pub fn push(&mut self, component: Component) {
        self.items.push(Item::Component(component));
    }

    pub fn retain_components(&mut self, mut keep: impl FnMut(&Component) -> bool) {
        self.items.retain(|i| match i {
            Item::Component(c) => keep(c),
            _ => true,
        });
    }
}

impl Property {
    /// A property with an already-escaped value.
    pub fn new(name: &str, value: impl Into<String>) -> Property {
        Property {
            name: name.to_string(),
            params: Vec::new(),
            value: value.into(),
            raw: None,
        }
    }

    /// A TEXT property; the value is escaped.
    pub fn text(name: &str, text: &str) -> Property {
        Property::new(name, escape_text(text))
    }

    pub fn with_param(mut self, name: &str, value: &str) -> Property {
        self.params.retain(|p| !p.name.eq_ignore_ascii_case(name));
        self.params.push(Param {
            name: name.to_string(),
            values: vec![value.to_string()],
        });
        self.raw = None;
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .and_then(|p| p.values.first())
            .map(String::as_str)
    }

    /// The raw (escaped) value.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// The value as TEXT, unescaped.
    pub fn text_value(&self) -> String {
        unescape_text(&self.value)
    }

    /// A comma-separated TEXT list (CATEGORIES), unescaped.
    pub fn text_values(&self) -> Vec<String> {
        split_unescaped_commas(&self.value)
            .into_iter()
            .map(unescape_text)
            .collect()
    }

    fn same_content(&self, other: &Property) -> bool {
        self.name.eq_ignore_ascii_case(&other.name)
            && self.params == other.params
            && self.value == other.value
    }

    fn render(&self) -> String {
        let mut line = self.name.clone();
        for p in &self.params {
            line.push(';');
            line.push_str(&p.name);
            line.push('=');
            for (i, v) in p.values.iter().enumerate() {
                if i > 0 {
                    line.push(',');
                }
                if v.contains([':', ';', ',']) {
                    line.push('"');
                    line.push_str(v);
                    line.push('"');
                } else {
                    line.push_str(v);
                }
            }
        }
        line.push(':');
        line.push_str(&self.value);
        line
    }
}

struct LogicalLine {
    raw: String,
    unfolded: String,
}

fn logical_lines(input: &str) -> Vec<LogicalLine> {
    let mut lines: Vec<LogicalLine> = Vec::new();
    for physical in input.split_inclusive('\n') {
        let content = physical.trim_end_matches(['\n', '\r']);
        let is_continuation = content.starts_with([' ', '\t']);
        match lines.last_mut() {
            Some(last) if is_continuation && !last.unfolded.is_empty() => {
                last.raw.push_str(physical);
                last.unfolded.push_str(&content[1..]);
            }
            _ => lines.push(LogicalLine {
                raw: physical.to_string(),
                unfolded: content.to_string(),
            }),
        }
    }
    lines
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-'
}

fn parse_line(line: &str, raw: &str) -> Option<Property> {
    let name_end = line.find(|c: char| !is_name_char(c)).unwrap_or(line.len());
    if name_end == 0 {
        return None;
    }
    let name = &line[..name_end];
    let mut rest = &line[name_end..];
    let mut params = Vec::new();
    loop {
        if let Some(value) = rest.strip_prefix(':') {
            return Some(Property {
                name: name.to_string(),
                params,
                value: value.to_string(),
                raw: Some(raw.to_string()),
            });
        }
        rest = rest.strip_prefix(';')?;
        let pname_end = rest.find(|c: char| !is_name_char(c)).unwrap_or(rest.len());
        if pname_end == 0 {
            return None;
        }
        let pname = &rest[..pname_end];
        rest = rest[pname_end..].strip_prefix('=')?;
        let mut values = Vec::new();
        loop {
            if let Some(quoted) = rest.strip_prefix('"') {
                let close = quoted.find('"')?;
                values.push(quoted[..close].to_string());
                rest = &quoted[close + 1..];
            } else {
                let end = rest.find([',', ';', ':']).unwrap_or(rest.len());
                values.push(rest[..end].to_string());
                rest = &rest[end..];
            }
            match rest.strip_prefix(',') {
                Some(more) => rest = more,
                None => break,
            }
        }
        params.push(Param {
            name: pname.to_string(),
            values,
        });
    }
}

fn write_items(items: &[Item], out: &mut String) {
    for item in items {
        match item {
            Item::Property(p) => match &p.raw {
                Some(raw) => out.push_str(raw),
                None => fold_into(&p.render(), out),
            },
            Item::Component(c) => write_component(c, out),
            Item::Junk(raw) => out.push_str(raw),
        }
    }
}

fn write_component(c: &Component, out: &mut String) {
    match &c.begin_raw {
        Some(raw) => out.push_str(raw),
        None => fold_into(&format!("BEGIN:{}", c.name), out),
    }
    write_items(&c.items, out);
    match &c.end_raw {
        Some(raw) => out.push_str(raw),
        None => fold_into(&format!("END:{}", c.name), out),
    }
}

/// Write one content line folded at 75 octets, never inside a UTF-8 sequence.
fn fold_into(line: &str, out: &mut String) {
    let mut start = 0;
    let mut first = true;
    loop {
        let limit = if first { 75 } else { 74 };
        let mut end = (start + limit).min(line.len());
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        if !first {
            out.push(' ');
        }
        out.push_str(&line[start..end]);
        out.push_str("\r\n");
        start = end;
        first = false;
        if start >= line.len() {
            break;
        }
    }
}

pub fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\r' => {
                if chars.peek() != Some(&'\n') {
                    out.push_str("\\n");
                }
            }
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

pub fn unescape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

fn split_unescaped_commas(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut escaped = false;
    for (i, c) in value.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            ',' => {
                parts.push(&value[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&value[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shaped like what iOS writes: folded text, a VTIMEZONE, an alarm, a
    // quoted parameter holding ':' and ',', and properties asst never reads.
    const IOS: &str = "BEGIN:VCALENDAR\r\n\
CALSCALE:GREGORIAN\r\n\
PRODID:-//Apple Inc.//iOS 18.6//EN\r\n\
VERSION:2.0\r\n\
BEGIN:VTIMEZONE\r\n\
TZID:America/New_York\r\n\
BEGIN:DAYLIGHT\r\n\
DTSTART:20070311T020000\r\n\
RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=2SU\r\n\
TZNAME:EDT\r\n\
TZOFFSETFROM:-0500\r\n\
TZOFFSETTO:-0400\r\n\
END:DAYLIGHT\r\n\
BEGIN:STANDARD\r\n\
DTSTART:20071104T020000\r\n\
RRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU\r\n\
TZNAME:EST\r\n\
TZOFFSETFROM:-0400\r\n\
TZOFFSETTO:-0500\r\n\
END:STANDARD\r\n\
END:VTIMEZONE\r\n\
BEGIN:VTODO\r\n\
CREATED:20260901T120000Z\r\n\
DESCRIPTION:Bring the charger\\, the adapter\\; and the long cable that l\r\n\
\x20ives in the drawer — 전원\r\n\
DTSTAMP:20260901T120500Z\r\n\
DTSTART;TZID=America/New_York:20260915T090000\r\n\
DUE;TZID=America/New_York:20260915T090000\r\n\
LAST-MODIFIED:20260901T120500Z\r\n\
PRIORITY:5\r\n\
SEQUENCE:0\r\n\
SUMMARY:Pack for the trip\r\n\
UID:0F276A13-FBF3-49A1-8369-65EEA9C6F891\r\n\
X-APPLE-SORT-ORDER:28\r\n\
X-APPLE-STRUCTURED-LOCATION;VALUE=URI;X-ADDRESS=\"1 Main St, Springfield\";X-TITLE=Home:geo:40.7,-74.0\r\n\
BEGIN:VALARM\r\n\
ACTION:DISPLAY\r\n\
DESCRIPTION:Reminder\r\n\
TRIGGER;VALUE=DATE-TIME:20260915T130000Z\r\n\
UID:6C2F3A0B-1A5E-4F4B-9A7C-3E0E7E0D2B11\r\n\
X-WR-ALARMUID:6C2F3A0B-1A5E-4F4B-9A7C-3E0E7E0D2B11\r\n\
END:VALARM\r\n\
END:VTODO\r\n\
END:VCALENDAR\r\n";

    #[test]
    fn round_trip_is_byte_identical() {
        assert_eq!(Ical::parse(IOS).to_string(), IOS);
    }

    #[test]
    fn round_trip_keeps_lf_endings_and_junk() {
        let odd = "BEGIN:VCALENDAR\nVERSION:2.0\nnot a content line\n\nBEGIN:VTODO\nUID:x\nEND:VTODO\nEND:VCALENDAR";
        assert_eq!(Ical::parse(odd).to_string(), odd);
    }

    #[test]
    fn unclosed_components_stay_unclosed() {
        let cut = "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:x\r\n";
        let ical = Ical::parse(cut);
        assert_eq!(ical.to_string(), cut);
        assert_eq!(ical.todo().unwrap().property("UID").unwrap().value(), "x");
    }

    #[test]
    fn reads_values_params_and_unfolds() {
        let ical = Ical::parse(IOS);
        let todo = ical.todo().unwrap();
        assert_eq!(
            todo.property("description").unwrap().text_value(),
            "Bring the charger, the adapter; and the long cable that lives in the drawer — 전원"
        );
        assert_eq!(
            todo.property("DUE").unwrap().param("tzid"),
            Some("America/New_York")
        );
        let loc = todo.property("X-APPLE-STRUCTURED-LOCATION").unwrap();
        assert_eq!(loc.param("X-ADDRESS"), Some("1 Main St, Springfield"));
        assert_eq!(loc.value(), "geo:40.7,-74.0");
        assert_eq!(todo.components().count(), 1);
    }

    #[test]
    fn setting_one_property_changes_only_its_line() {
        let mut ical = Ical::parse(IOS);
        ical.todo_mut()
            .unwrap()
            .set(Property::text("SUMMARY", "Pack, then leave"));
        let expected = IOS.replace("SUMMARY:Pack for the trip", "SUMMARY:Pack\\, then leave");
        assert_eq!(ical.to_string(), expected);
    }

    #[test]
    fn setting_an_equal_value_keeps_the_original_bytes() {
        let mut ical = Ical::parse(IOS);
        ical.todo_mut().unwrap().set(Property::new("PRIORITY", "5"));
        assert_eq!(ical.to_string(), IOS);
    }

    #[test]
    fn new_properties_go_before_subcomponents() {
        let mut ical = Ical::parse(IOS);
        let todo = ical.todo_mut().unwrap();
        todo.set(Property::new("STATUS", "COMPLETED"));
        assert!(todo.remove("SEQUENCE"));
        let out = ical.to_string();
        let expected = IOS.replace("SEQUENCE:0\r\n", "").replace(
            "Home:geo:40.7,-74.0\r\n",
            "Home:geo:40.7,-74.0\r\nSTATUS:COMPLETED\r\n",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn duplicates_collapse_on_set() {
        let mut ical = Ical::parse(
            "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nSUMMARY:a\r\nUID:1\r\nSUMMARY:b\r\nEND:VTODO\r\nEND:VCALENDAR\r\n",
        );
        ical.todo_mut().unwrap().set(Property::text("SUMMARY", "c"));
        assert_eq!(
            ical.to_string(),
            "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nSUMMARY:c\r\nUID:1\r\nEND:VTODO\r\nEND:VCALENDAR\r\n"
        );
    }

    #[test]
    fn long_lines_fold_at_75_octets_on_char_boundaries() {
        let text = "가".repeat(60); // 3 bytes each
        let mut out = String::new();
        fold_into(&format!("SUMMARY:{text}"), &mut out);
        for line in out.split("\r\n").filter(|l| !l.is_empty()) {
            assert!(line.len() <= 75, "{} octets", line.len());
        }
        let back = Ical::parse(&format!(
            "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\n{out}END:VTODO\r\nEND:VCALENDAR\r\n"
        ));
        assert_eq!(
            back.todo()
                .unwrap()
                .property("SUMMARY")
                .unwrap()
                .text_value(),
            text
        );
    }

    #[test]
    fn new_components_render_with_quoted_params() {
        let mut cal = Component::new("VCALENDAR");
        let mut todo = Component::new("VTODO");
        todo.set(Property::new("X-TEST", "v").with_param("X-NOTE", "a:b"));
        cal.push(todo);
        assert_eq!(
            Ical::new(cal).to_string(),
            "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nX-TEST;X-NOTE=\"a:b\":v\r\nEND:VTODO\r\nEND:VCALENDAR\r\n"
        );
    }

    #[test]
    fn text_escaping_round_trips() {
        let s = "a\\b;c,d\ne\r\nf";
        assert_eq!(unescape_text(&escape_text(s)), "a\\b;c,d\ne\nf");
        let cats = Property::new("CATEGORIES", "Home\\, garden,Work");
        assert_eq!(cats.text_values(), vec!["Home, garden", "Work"]);
    }
}
