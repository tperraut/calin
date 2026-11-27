//! Validation problems.

use std::fmt;

use serde::{Deserialize, Serialize};

/// What a [`Problem`] is about, so a UI can point at it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Subject {
    /// The document as a whole, or one of its top-level keys.
    Workflow,
    /// A node, by id. A node without a usable id is named `#<position>` (1-based).
    Node {
        /// The node's id, or `#<position>` when it has none.
        id: String,
    },
    /// An edge, by its 0-based position in `edges`.
    Edge {
        /// Position in the file's `edges` list, 0-based.
        index: usize,
    },
}

/// One thing wrong with a workflow file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    /// 1-based line in the YAML source, when known.
    pub line: Option<u32>,
    /// What the problem is about.
    pub subject: Subject,
    /// A sentence for a person. It names the node, edge or key it is about.
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "line {line}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

/// Every problem found in a workflow, in file order where it has one. Never empty when
/// returned as an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub struct Problems(pub Vec<Problem>);

impl Problems {
    /// The problems, in the order they were found.
    pub fn iter(&self) -> std::slice::Iter<'_, Problem> {
        self.0.iter()
    }

    /// How many problems there are.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none. A `Problems` returned as an error is never empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for Problems {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, p) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{p}")?;
        }
        Ok(())
    }
}

impl<'a> IntoIterator for &'a Problems {
    type Item = &'a Problem;
    type IntoIter = std::slice::Iter<'a, Problem>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// Collects problems during one pass.
#[derive(Default)]
pub(crate) struct Collector {
    pub problems: Vec<Problem>,
}

impl Collector {
    pub fn push(&mut self, line: Option<u32>, subject: Subject, message: impl Into<String>) {
        self.problems.push(Problem {
            line,
            subject,
            message: message.into(),
        });
    }

    pub fn is_empty(&self) -> bool {
        self.problems.is_empty()
    }

    /// Problems sorted by line (stable, so same-line problems keep discovery order).
    /// Problems without a line go last.
    pub fn finish(mut self) -> Problems {
        self.problems
            .sort_by_key(|p| p.line.map_or((1, 0), |l| (0, l)));
        Problems(self.problems)
    }
}
