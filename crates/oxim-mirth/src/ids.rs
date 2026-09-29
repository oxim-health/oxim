//! OXIM identifiers from Mirth names.

use std::collections::BTreeSet;

/// Longest OXIM channel or connector identifier.
const MAX_LEN: usize = 64;

/// A valid OXIM identifier made from `name`: lowercase ASCII letters and
/// digits with `-` between words, or `fallback` when nothing is left.
pub(crate) fn slug(name: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(c.to_ascii_lowercase());
        } else {
            dash = true;
        }
    }
    out.truncate(MAX_LEN - 4);
    let out = out.trim_end_matches('-').to_owned();
    if out.is_empty() {
        fallback.to_owned()
    } else {
        out
    }
}

/// Hands out identifiers that are unique within one scope.
#[derive(Debug, Default)]
pub(crate) struct Names {
    used: BTreeSet<String>,
}

impl Names {
    /// Marks `name` as taken.
    pub(crate) fn reserve(&mut self, name: &str) {
        self.used.insert(name.to_owned());
    }

    /// `base`, or `base-2`, `base-3`, ... when taken.
    pub(crate) fn unique(&mut self, base: &str) -> String {
        if self.used.insert(base.to_owned()) {
            return base.to_owned();
        }
        let mut n = 2usize;
        loop {
            let candidate = format!("{base}-{n}");
            if self.used.insert(candidate.clone()) {
                return candidate;
            }
            n += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_valid_unique_identifiers() {
        assert_eq!(slug("ADT Inbound (v2)", "channel"), "adt-inbound-v2");
        assert_eq!(slug("  Lab -> LIS  ", "channel"), "lab-lis");
        assert_eq!(slug("Çağrı", "channel"), "a-r");
        assert_eq!(slug("***", "channel"), "channel");
        assert!(slug(&"x".repeat(200), "c").len() <= MAX_LEN - 4);
        let mut names = Names::default();
        names.reserve("source");
        assert_eq!(names.unique("source"), "source-2");
        assert_eq!(names.unique("lis"), "lis");
        assert_eq!(names.unique("lis"), "lis-2");
        assert_eq!(names.unique("lis"), "lis-3");
        for id in ["adt-inbound-v2", "lab-lis", "a-r", "source-2"] {
            assert!(
                oxim_core::ChannelConfig::from_yaml(&format!(
                    "id: {id}\nsource: {{type: x, data_type: raw}}\n"
                ))
                .is_ok()
            );
        }
    }
}
