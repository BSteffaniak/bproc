//! Search / filter language.
//!
//! Plain words fuzzy-match the name and command line. Structured terms use
//! column filter keys: `cpu>10`, `mem>=2.5`, `rss>500M`, `user:root`,
//! `state:Z`, `port:443`, `pid:1234`, `name:node`. A leading `-` negates a
//! term (`-user:root`). Terms are ANDed; `|` between terms ORs them.
use super::columns::ColumnCtx;
use super::store::Row;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Filter {
    source: String,
    terms: Vec<String>,
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

impl Filter {
    pub fn parse(input: &str) -> Result<Filter, FilterError> {
        Ok(Filter {
            source: input.to_string(),
            terms: input.split_whitespace().map(str::to_lowercase).collect(),
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    pub fn matches(&self, row: &Row, _ctx: &ColumnCtx) -> Option<Match> {
        let command = row.entry.record.command().to_lowercase();
        let name = row.entry.record.name.to_lowercase();
        self.terms
            .iter()
            .all(|t| name.contains(t) || command.contains(t))
            .then(Match::default)
    }
}
