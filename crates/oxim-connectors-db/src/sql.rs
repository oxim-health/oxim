//! Named parameters (`:name`) in SQL, rewritten to each database's
//! placeholders.

/// How a database writes positional placeholders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placeholder {
    /// PostgreSQL: `$1`, `$2`; a repeated name reuses its number.
    Dollar,
    /// MySQL and MariaDB: `?`; a repeated name is bound again.
    Question,
    /// SQL Server: `@P1`, `@P2`; a repeated name reuses its number.
    AtP,
    /// SQLite: `?1`, `?2`; a repeated name reuses its number.
    NumberedQuestion,
}

impl Placeholder {
    /// Whether a backslash escapes the next character inside quotes.
    fn backslash_escapes(self) -> bool {
        self == Self::Question
    }
}

/// A statement with its placeholders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Prepared {
    /// The SQL with database placeholders.
    pub(crate) sql: String,
    /// The parameter bound to each placeholder position, in order.
    pub(crate) names: Vec<String>,
}

fn ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Rewrites the `:name` parameters of `sql` for `style`. Quoted text,
/// quoted identifiers, comments, PostgreSQL `::` casts and dollar-quoted
/// strings are left alone. Positional placeholders written by hand are
/// rejected, so every parameter is named and bound by name.
pub(crate) fn prepare(sql: &str, style: Placeholder) -> Result<Prepared, String> {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len() + 8);
    let mut names: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '\'' | '"' | '`' => {
                let escapes = style.backslash_escapes()
                    || (c == '\''
                        && i > 0
                        && matches!(chars[i - 1], 'e' | 'E')
                        && !chars
                            .get(i.wrapping_sub(2))
                            .copied()
                            .is_some_and(ident_char));
                let end = skip_quoted(&chars, i, c, c, escapes)?;
                out.extend(&chars[i..end]);
                i = end;
            }
            '[' if style == Placeholder::AtP => {
                let end = skip_quoted(&chars, i, '[', ']', false)?;
                out.extend(&chars[i..end]);
                i = end;
            }
            '-' if next == Some('-') => {
                let end = chars[i..]
                    .iter()
                    .position(|&c| c == '\n')
                    .map_or(chars.len(), |p| i + p);
                out.extend(&chars[i..end]);
                i = end;
            }
            '#' if style == Placeholder::Question => {
                let end = chars[i..]
                    .iter()
                    .position(|&c| c == '\n')
                    .map_or(chars.len(), |p| i + p);
                out.extend(&chars[i..end]);
                i = end;
            }
            '/' if next == Some('*') => {
                let end = (i + 2..chars.len().saturating_sub(1))
                    .find(|&j| chars[j] == '*' && chars[j + 1] == '/')
                    .map(|j| j + 2)
                    .ok_or("an unterminated /* comment")?;
                out.extend(&chars[i..end]);
                i = end;
            }
            '$' if style == Placeholder::Dollar => {
                if next.is_some_and(|c| c.is_ascii_digit()) {
                    return Err("write named parameters (:name) instead of $1".into());
                }
                // A dollar-quoted string: $$...$$ or $tag$...$tag$.
                let tag_end = (i + 1..chars.len())
                    .take_while(|&j| chars[j] == '$' || ident_char(chars[j]))
                    .find(|&j| chars[j] == '$');
                // Inside an identifier (`a$b`) a dollar is an ordinary
                // character, not the start of a quote.
                let in_identifier = i > 0 && ident_char(chars[i - 1]);
                match tag_end {
                    Some(t) if !in_identifier && chars[i + 1..t].iter().all(|&c| ident_char(c)) => {
                        let tag: String = chars[i..=t].iter().collect();
                        let body: String = chars[t + 1..].iter().collect();
                        let close = body
                            .find(&tag)
                            .ok_or_else(|| format!("an unterminated {tag} string"))?;
                        let end = t + 1 + body[..close].chars().count() + tag.chars().count();
                        out.extend(&chars[i..end]);
                        i = end;
                    }
                    _ => {
                        out.push(c);
                        i += 1;
                    }
                }
            }
            '?' if matches!(style, Placeholder::Question | Placeholder::NumberedQuestion) => {
                return Err("write named parameters (:name) instead of ?".into());
            }
            ':' if next == Some(':') => {
                out.push_str("::");
                i += 2;
            }
            ':' if next.is_some_and(ident_start) => {
                let end = (i + 1..chars.len())
                    .find(|&j| !ident_char(chars[j]))
                    .unwrap_or(chars.len());
                let name: String = chars[i + 1..end].iter().collect();
                let position = match style {
                    Placeholder::Question => {
                        names.push(name);
                        names.len()
                    }
                    _ => match names.iter().position(|n| *n == name) {
                        Some(p) => p + 1,
                        None => {
                            names.push(name);
                            names.len()
                        }
                    },
                };
                match style {
                    Placeholder::Dollar => out.push_str(&format!("${position}")),
                    Placeholder::Question => out.push('?'),
                    Placeholder::AtP => out.push_str(&format!("@P{position}")),
                    Placeholder::NumberedQuestion => out.push_str(&format!("?{position}")),
                }
                i = end;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(Prepared { sql: out, names })
}

/// The index just past a quoted section that starts at `start`.
fn skip_quoted(
    chars: &[char],
    start: usize,
    open: char,
    close: char,
    backslash_escapes: bool,
) -> Result<usize, String> {
    let mut j = start + 1;
    while j < chars.len() {
        let c = chars[j];
        if backslash_escapes && c == '\\' {
            j += 2;
            continue;
        }
        if c == close {
            // A doubled closing character is an escaped one.
            if chars.get(j + 1) == Some(&close) {
                j += 2;
                continue;
            }
            return Ok(j + 1);
        }
        j += 1;
    }
    Err(format!("an unterminated {open}...{close} section"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(sql: &str, style: Placeholder) -> (String, Vec<String>) {
        let prepared = prepare(sql, style).unwrap();
        (prepared.sql, prepared.names)
    }

    #[test]
    fn rewrites_for_each_database() {
        let sql = "INSERT INTO results (id, mrn, again) VALUES (:id, :mrn, :id)";
        assert_eq!(
            run(sql, Placeholder::Dollar),
            (
                "INSERT INTO results (id, mrn, again) VALUES ($1, $2, $1)".into(),
                vec!["id".into(), "mrn".into()]
            )
        );
        assert_eq!(
            run(sql, Placeholder::Question),
            (
                "INSERT INTO results (id, mrn, again) VALUES (?, ?, ?)".into(),
                vec!["id".into(), "mrn".into(), "id".into()]
            )
        );
        assert_eq!(
            run(sql, Placeholder::AtP).0,
            "INSERT INTO results (id, mrn, again) VALUES (@P1, @P2, @P1)"
        );
        assert_eq!(
            run(sql, Placeholder::NumberedQuestion).0,
            "INSERT INTO results (id, mrn, again) VALUES (?1, ?2, ?1)"
        );
    }

    #[test]
    fn leaves_quotes_comments_and_casts_alone() {
        let sql = "SELECT ':not', \"col:x\", ':a''b:c', x::text, $$ :d $$, $t$ :e $t$ -- :f\n/* :g */ FROM t WHERE y = :y";
        let (out, names) = run(sql, Placeholder::Dollar);
        assert_eq!(names, ["y"]);
        assert!(out.ends_with("WHERE y = $1"), "{out}");
        assert!(out.contains("x::text"));
        assert!(out.contains("$t$ :e $t$"));

        // MySQL: backslash escapes and # comments.
        let (out, names) = run(
            r"SELECT 'it\'s :no', `a:b` # :c
FROM t WHERE z = :z",
            Placeholder::Question,
        );
        assert_eq!(names, ["z"]);
        assert!(out.ends_with("z = ?"), "{out}");

        // SQL Server: bracketed identifiers and variables.
        let (out, names) = run(
            "SELECT [col:x] FROM t WHERE @v = 1 AND k = :k",
            Placeholder::AtP,
        );
        assert_eq!(names, ["k"]);
        assert!(out.contains("[col:x]") && out.ends_with("k = @P1"), "{out}");

        // PostgreSQL escape strings keep a backslash-escaped quote inside.
        let (_, names) = run(r"SELECT E'a\':b' || :v", Placeholder::Dollar);
        assert_eq!(names, ["v"]);
    }

    #[test]
    fn rejects_positional_and_unterminated_sections() {
        assert!(prepare("SELECT $1", Placeholder::Dollar).is_err());
        assert!(prepare("SELECT ?", Placeholder::Question).is_err());
        assert!(prepare("SELECT ?1", Placeholder::NumberedQuestion).is_err());
        assert!(prepare("SELECT 'open", Placeholder::Dollar).is_err());
        assert!(prepare("SELECT /* open", Placeholder::Dollar).is_err());
        assert!(prepare("SELECT $x$ open", Placeholder::Dollar).is_err());
        // A dollar inside an identifier is not a dollar quote.
        assert_eq!(run("SELECT a$b", Placeholder::Dollar).0, "SELECT a$b");
        assert_eq!(
            run("SELECT a$b$c, :p", Placeholder::Dollar).0,
            "SELECT a$b$c, $1"
        );
        // := (MySQL assignment) is not a parameter.
        assert!(run("SET @a := 1", Placeholder::Question).1.is_empty());
    }
}
