//! Domain: a minimal JSON scanner that finds object members by byte span, so a file can be
//! edited in place without reformatting anything else. Pure.
//!
//! The text must already be valid JSON (callers validate with `serde_json` first); the scanner
//! only locates spans and reports an error instead of panicking on anything else.

use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unexpected JSON structure at byte {0}")]
pub struct ScanError(usize);

/// One `"key": value` pair of an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub key: String,
    /// Byte offset of the opening quote of the key.
    pub start: usize,
    /// Byte range of the value, without surrounding whitespace.
    pub value: Range<usize>,
}

/// Members of the object whose `{` is the first non-space byte at or after `at`,
/// and the byte range of the whole object including both braces.
pub fn parse_object(text: &str, at: usize) -> Result<(Vec<Member>, Range<usize>), ScanError> {
    let mut s = Scan {
        text,
        i: at,
        depth: 0,
    };
    s.ws();
    let start = s.i;
    let members = s.object()?;
    Ok((members, start..s.i))
}

/// Element spans of the array whose `[` is the first non-space byte at or after `at`,
/// and the byte range of the whole array including both brackets.
pub fn parse_array(text: &str, at: usize) -> Result<(Vec<Range<usize>>, Range<usize>), ScanError> {
    let mut s = Scan {
        text,
        i: at,
        depth: 0,
    };
    s.ws();
    let start = s.i;
    let items = s.array()?;
    Ok((items, start..s.i))
}

struct Scan<'a> {
    text: &'a str,
    i: usize,
    depth: usize,
}

impl Scan<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.i).copied()
    }

    fn err<T>(&self) -> Result<T, ScanError> {
        Err(ScanError(self.i))
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> Result<(), ScanError> {
        if self.peek() == Some(byte) {
            self.i += 1;
            Ok(())
        } else {
            self.err()
        }
    }

    /// Byte range of a string literal including its quotes.
    fn string(&mut self) -> Result<Range<usize>, ScanError> {
        let start = self.i;
        self.eat(b'"')?;
        loop {
            match self.peek() {
                None => return self.err(),
                Some(b'\\') => self.i += 2,
                Some(b'"') => {
                    self.i += 1;
                    return Ok(start..self.i);
                }
                Some(_) => self.i += 1,
            }
        }
    }

    fn value(&mut self) -> Result<Range<usize>, ScanError> {
        self.ws();
        let start = self.i;
        match self.peek() {
            Some(b'{') => {
                self.object()?;
            }
            Some(b'[') => {
                self.array()?;
            }
            Some(b'"') => {
                self.string()?;
            }
            Some(_) => {
                while !matches!(
                    self.peek(),
                    None | Some(b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
                ) {
                    self.i += 1;
                }
            }
            None => return self.err(),
        }
        Ok(start..self.i)
    }

    fn nested<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, ScanError>,
    ) -> Result<T, ScanError> {
        self.depth += 1;
        if self.depth > 128 {
            return self.err();
        }
        let out = f(self);
        self.depth -= 1;
        out
    }

    fn object(&mut self) -> Result<Vec<Member>, ScanError> {
        self.nested(|s| {
            s.eat(b'{')?;
            let mut members = Vec::new();
            s.ws();
            if s.peek() == Some(b'}') {
                s.i += 1;
                return Ok(members);
            }
            loop {
                s.ws();
                let key_span = s.string()?;
                let key = serde_json::from_str(&s.text[key_span.clone()]).or_else(|_| s.err())?;
                s.ws();
                s.eat(b':')?;
                let value = s.value()?;
                members.push(Member {
                    key,
                    start: key_span.start,
                    value,
                });
                s.ws();
                match s.peek() {
                    Some(b',') => s.i += 1,
                    Some(b'}') => {
                        s.i += 1;
                        return Ok(members);
                    }
                    _ => return s.err(),
                }
            }
        })
    }

    fn array(&mut self) -> Result<Vec<Range<usize>>, ScanError> {
        self.nested(|s| {
            s.eat(b'[')?;
            let mut items = Vec::new();
            s.ws();
            if s.peek() == Some(b']') {
                s.i += 1;
                return Ok(items);
            }
            loop {
                items.push(s.value()?);
                s.ws();
                match s.peek() {
                    Some(b',') => s.i += 1,
                    Some(b']') => {
                        s.i += 1;
                        return Ok(items);
                    }
                    _ => return s.err(),
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locates_members_and_skips_nested_values() {
        let text = "  {\n  \"a\": [1, {\"x\": \"}\\\"\"}],\n  \"b\\u0041\": true ,\"c\":{}\n}\n";
        let (members, whole) = parse_object(text, 0).unwrap();
        let keys: Vec<&str> = members.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(keys, ["a", "bA", "c"]);
        assert_eq!(&text[members[0].value.clone()], "[1, {\"x\": \"}\\\"\"}]");
        assert_eq!(&text[members[1].value.clone()], "true");
        assert_eq!(&text[members[2].value.clone()], "{}");
        assert_eq!(&text[whole], &text[2..text.len() - 1]);
    }

    #[test]
    fn locates_array_elements_by_span() {
        let text = "x [1, {\"a\": [2]},\n \"s]\" ] y";
        let (items, whole) = parse_array(text, 2).unwrap();
        let got: Vec<&str> = items.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(got, ["1", "{\"a\": [2]}", "\"s]\""]);
        assert_eq!(&text[whole], "[1, {\"a\": [2]},\n \"s]\" ]");
        assert_eq!(parse_array("[ ]", 0).unwrap().0, Vec::<Range<usize>>::new());
    }

    #[test]
    fn broken_input_is_an_error_not_a_panic() {
        for bad in ["{", "{\"a\"", "{\"a\":", "{\"a\": [1,", "[1]", ""] {
            assert!(parse_object(bad, 0).is_err(), "{bad}");
        }
        for bad in ["[", "[1,", "{}", ""] {
            assert!(parse_array(bad, 0).is_err(), "{bad}");
        }
    }
}
