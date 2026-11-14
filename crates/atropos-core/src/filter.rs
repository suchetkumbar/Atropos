//! Search bar (spec Phase 1): `pid:123`, `user:me`, `name:node`, `cwd:shop` plus free text,
//! all AND-combined. Free text is fuzzy-matched (`nucleo-matcher`) over label, image, command
//! line and PID. Parsing never fails: anything unrecognised is free text.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// One structured term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    Pid(u32),
    UserMe,
    /// Case-insensitive substring of the image name (or label).
    Name(String),
    /// Case-insensitive substring of the working directory (ignored while the cwd is unknown).
    Cwd(String),
    /// `:3000`: owns a socket on that local port (TCP or UDP).
    Port(u16),
    /// Free text (fuzzy).
    Text(String),
}

/// What a filter is matched against; fields that are unknown are `None`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Subject<'a> {
    pub pid: u32,
    pub image: &'a str,
    pub label: Option<&'a str>,
    pub cmdline: Option<&'a str>,
    pub user: Option<&'a str>,
    pub cwd: Option<&'a str>,
    /// Local ports this process owns; `None` while sockets are not loaded (a port term then
    /// matches nothing rather than everything).
    pub ports: Option<&'a [u16]>,
}

/// A parsed search string.
#[derive(Debug, Default)]
pub struct Filter {
    terms: Vec<Term>,
    fuzzy: Option<Pattern>,
}

/// Reusable matcher state (allocation-free per row).
pub struct FilterMatcher {
    matcher: Matcher,
    hay: String,
    buf: Vec<char>,
}

impl Default for FilterMatcher {
    fn default() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
            hay: String::new(),
            buf: Vec::new(),
        }
    }
}

impl Filter {
    /// Parses `s`. Never fails.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        let terms: Vec<Term> = s.split_whitespace().map(parse_term).collect();
        let text: Vec<&str> = terms
            .iter()
            .filter_map(|t| match t {
                Term::Text(w) => Some(w.as_str()),
                _ => None,
            })
            .collect();
        let fuzzy = (!text.is_empty())
            .then(|| Pattern::parse(&text.join(" "), CaseMatching::Ignore, Normalization::Smart));
        Self { terms, fuzzy }
    }

    #[must_use]
    pub fn terms(&self) -> &[Term] {
        &self.terms
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Whether matching needs socket data (`:port` terms).
    #[must_use]
    pub fn has_port_term(&self) -> bool {
        self.terms.iter().any(|t| matches!(t, Term::Port(_)))
    }

    /// `Some(score)` if every term matches (higher is better; 0 without free text).
    /// `me` is the current user's account name, for `user:me`.
    pub fn matches(&self, s: &Subject<'_>, me: &str, m: &mut FilterMatcher) -> Option<u32> {
        for term in &self.terms {
            let ok = match term {
                Term::Pid(pid) => s.pid == *pid,
                Term::UserMe => s
                    .user
                    .is_some_and(|u| u.to_lowercase() == me.to_lowercase()),
                Term::Name(n) => {
                    contains_ci(s.image, n) || s.label.is_some_and(|l| contains_ci(l, n))
                }
                Term::Cwd(c) => s.cwd.is_none_or(|cwd| contains_ci(cwd, c)),
                Term::Port(port) => s.ports.is_some_and(|ps| ps.contains(port)),
                Term::Text(_) => true,
            };
            if !ok {
                return None;
            }
        }
        let Some(fuzzy) = &self.fuzzy else {
            return Some(0);
        };
        m.hay.clear();
        for part in [s.label, Some(s.image), s.cmdline].into_iter().flatten() {
            m.hay.push_str(part);
            m.hay.push(' ');
        }
        m.hay.push_str(&s.pid.to_string());
        fuzzy.score(Utf32Str::new(&m.hay, &mut m.buf), &mut m.matcher)
    }
}

fn contains_ci(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(&needle.to_lowercase())
}

fn parse_term(word: &str) -> Term {
    let text = || Term::Text(word.to_owned());
    if let Some(port) = word.strip_prefix(':') {
        return port.parse().map_or_else(|_| text(), Term::Port);
    }
    let Some((key, value)) = word.split_once(':') else {
        return text();
    };
    if value.is_empty() {
        return text();
    }
    match key.to_ascii_lowercase().as_str() {
        "pid" => value.parse().map_or_else(|_| text(), Term::Pid),
        "user" if value.eq_ignore_ascii_case("me") => Term::UserMe,
        "name" => Term::Name(value.to_owned()),
        "cwd" => Term::Cwd(value.to_owned()),
        _ => text(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Term {
        Term::Text(s.into())
    }

    #[test]
    fn parse_table() {
        let cases: &[(&str, Vec<Term>)] = &[
            ("", vec![]),
            ("   ", vec![]),
            ("pid:123", vec![Term::Pid(123)]),
            ("PID:7", vec![Term::Pid(7)]),
            ("user:me", vec![Term::UserMe]),
            ("name:node", vec![Term::Name("node".into())]),
            ("cwd:shop", vec![Term::Cwd("shop".into())]),
            ("node pid:12", vec![t("node"), Term::Pid(12)]),
            ("pid:abc", vec![t("pid:abc")]),
            ("pid:", vec![t("pid:")]),
            ("user:bob", vec![t("user:bob")]),
            ("color:red", vec![t("color:red")]),
            ("naïve name:Ünï", vec![t("naïve"), Term::Name("Ünï".into())]),
        ];
        for (input, want) in cases {
            assert_eq!(
                Filter::parse(input).terms(),
                want.as_slice(),
                "input {input:?}"
            );
        }
    }

    fn node() -> Subject<'static> {
        Subject {
            pid: 4242,
            image: "node.exe",
            label: Some("Next.js dev · shop"),
            cmdline: Some(r#""C:\Program Files\nodejs\node.exe" next dev --port 3000"#),
            user: Some("José"),
            cwd: Some(r"C:\src\Shop"),
            ports: None,
        }
    }

    fn hit(q: &str, s: &Subject<'_>) -> bool {
        Filter::parse(q)
            .matches(s, "josé", &mut FilterMatcher::default())
            .is_some()
    }

    #[test]
    fn empty_filter_matches_everything() {
        assert!(hit("", &node()));
    }

    #[test]
    fn structured_terms() {
        assert!(hit("pid:4242", &node()));
        assert!(!hit("pid:1", &node()));
        assert!(hit("name:NODE", &node()));
        assert!(!hit("name:python", &node()));
        assert!(hit("cwd:shop", &node()));
        assert!(!hit("cwd:blog", &node()));
    }

    #[test]
    fn user_me_matches_current_user_case_insensitively() {
        assert!(hit("user:me", &node()));
        let other = Subject {
            user: Some("SYSTEM"),
            ..node()
        };
        assert!(!hit("user:me", &other));
        let unknown = Subject {
            user: None,
            ..node()
        };
        assert!(!hit("user:me", &unknown));
    }

    #[test]
    fn and_combines_terms() {
        assert!(hit("name:node pid:4242", &node()));
        assert!(!hit("name:node pid:1", &node()));
        assert!(!hit("next zzzqqq", &node()));
    }

    #[test]
    fn fuzzy_text_searches_label_image_cmdline_and_pid() {
        assert!(hit("nxtdev", &node()));
        assert!(hit("3000", &node()));
        assert!(hit("4242", &node()));
        assert!(hit("SHOP", &node()));
        assert!(!hit("python", &node()));
    }

    #[test]
    fn unknown_cwd_ignores_the_cwd_term() {
        let s = Subject {
            cwd: None,
            ..node()
        };
        assert!(hit("cwd:anything", &s));
    }

    #[test]
    fn port_term_parses_and_matches_owned_ports() {
        assert_eq!(Filter::parse(":3000").terms(), &[Term::Port(3000)]);
        assert_eq!(Filter::parse(":abc").terms(), &[t(":abc")]);
        assert_eq!(Filter::parse(":70000").terms(), &[t(":70000")]);
        assert!(Filter::parse(":3000").has_port_term());
        assert!(!Filter::parse("node").has_port_term());
        let with = Subject {
            ports: Some(&[3000, 9229]),
            ..node()
        };
        assert!(hit(":3000", &with));
        assert!(hit(":9229 name:node", &with));
        assert!(!hit(":8080", &with));
        assert!(!hit(":3000", &node())); // sockets not loaded
    }

    #[test]
    fn unicode_text_matches() {
        let s = Subject {
            image: "naïve-ünï.exe",
            ..node()
        };
        assert!(hit("naïve", &s));
        assert!(hit("name:ÜNÏ", &s));
    }

    #[test]
    fn better_match_scores_higher() {
        let mut m = FilterMatcher::default();
        let f = Filter::parse("node");
        let exact = f.matches(&node(), "x", &mut m).unwrap();
        let weak = Subject {
            image: "n_o_d_e.exe",
            label: None,
            cmdline: None,
            ..node()
        };
        let loose = f.matches(&weak, "x", &mut m).unwrap();
        assert!(exact > loose);
    }
}
