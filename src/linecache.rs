//! The line cache (text, styles and cursors for a view).

use serde_json::Value;
use std::mem;
use std::ops::Range;

#[derive(Debug)]
pub struct Line {
    text: String,
    /// List of carets, in units of utf-16 code units.
    cursor: Vec<usize>,
    styles: Vec<StyleSpan>,
}

#[derive(Debug, Clone)]
pub struct StyleSpan {
    pub style_id: usize,
    /// Range of span, in units of utf-16 code units
    pub range: Range<usize>,
}

impl Line {
    // TODO(Olive) - This function should return Result<Self, SerdeErrorType>.
    pub fn from_json(v: &Value) -> Self {
        let text = v["text"].as_str().unwrap().to_owned();
        let mut cursor = vec![];
        if let Some(arr) = v["cursor"].as_array() {
            cursor = Vec::with_capacity(arr.len());
            for c in arr {
                let offset_utf8 = c.as_u64().unwrap() as usize;
                cursor.push(count_utf16(&text[..offset_utf8]));
            }
        }
        let mut styles = vec![];
        if let Some(arr) = v["styles"].as_array() {
            let mut ix: i64 = 0;
            for triple in arr.chunks(3) {
                let start = ix + triple[0].as_i64().unwrap();
                let end = start + triple[1].as_i64().unwrap();
                let style_id = triple[2].as_u64().unwrap() as usize;
                // TODO(Olive): count utf from last end, if <=
                let start_utf16 = count_utf16(&text[..start as usize]);
                let end_utf16 = start_utf16 + count_utf16(&text[start as usize..end as usize]);
                let style_span = StyleSpan {
                    style_id,
                    range: start_utf16..end_utf16,
                };
                styles.push(style_span);
                ix = end;
            }
        }
        Self {
            text,
            cursor,
            styles,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> &[usize] {
        &self.cursor
    }

    pub fn styles(&self) -> &[StyleSpan] {
        &self.styles
    }
}

#[derive(Debug, Default)]
pub struct LineCache {
    lines: Vec<Option<Line>>,
}

impl LineCache {
    pub const fn new() -> Self {
        Self { lines: vec![] }
    }

    fn push_opt_line(&mut self, line: Option<Line>) {
        self.lines.push(line);
    }

    pub fn apply_update(&mut self, update: &Value) {
        let old_cache = mem::take(self);
        let mut old_iter = old_cache.lines.into_iter();
        for op in update["ops"].as_array().unwrap() {
            let op_type = &op["op"];
            if op_type == "ins" {
                for line in op["lines"].as_array().unwrap() {
                    let line = Line::from_json(line);
                    self.push_opt_line(Some(line));
                }
            } else if op_type == "copy" {
                let n = op["n"].as_u64().unwrap();
                for _ in 0..n {
                    self.push_opt_line(old_iter.next().unwrap_or_default());
                }
            } else if op_type == "skip" {
                let n = op["n"].as_u64().unwrap().try_into().unwrap();
                let _skip = old_iter.nth(n);
            } else if op_type == "invalidate" {
                let n = op["n"].as_u64().unwrap();
                self.lines.extend((0..n).map(|_| None));
            }
        }
    }

    pub fn height(&self) -> usize {
        self.lines.len()
    }

    pub fn get_line(&self, i: usize) -> Option<&Line> {
        self.lines.get(i).and_then(Option::as_ref)
    }
}

/// Counts the number of utf-16 code units in the given string.
fn count_utf16(s: &str) -> usize {
    let mut utf16_count = 0;
    for &b in s.as_bytes() {
        if b as i8 >= -0x40 {
            utf16_count += 1;
        }
        if b >= 0xf0 {
            utf16_count += 1;
        }
    }
    utf16_count
}
