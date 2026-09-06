//! xpath addressing: type steps (`/field`) and instance steps (`[i]`).
//!
//! An xpath is an ordered list of [`Step`]s and is the unique addressing
//! vocabulary over a v5 tree. Every unique address has exactly one spelling:
//! named children (struct fields, `String` map keys, and `[u8; 16]` map keys)
//! are [`Step::Field`] steps (bytes16 keys use a lowercase 32-char hex field
//! name), while positional children are [`Step::Index`] steps (`[i]`). The
//! deterministic display form is the canonical serialization used as the input
//! to the xpath hash (N-8), so the canonical form is part of the protocol.

use std::fmt::{Display, Formatter};

/// One addressing step.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    /// A field access (`/name`); struct fields and map keys share this step.
    Field(String),
    /// An ordered container index (`[i]`).
    Index(usize),
}

/// An ordered xpath over a tree.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct XPath {
    steps: Vec<Step>,
}

impl XPath {
    /// A root xpath (no steps).
    pub fn root() -> Self {
        Self { steps: Vec::new() }
    }

    /// Constructs from explicit steps.
    pub fn from_steps(steps: Vec<Step>) -> Self {
        Self { steps }
    }

    /// Parses a canonical xpath string like `/a/b[3]/c`.
    pub fn parse(input: &str) -> Result<Self, XPathError> {
        if input.is_empty() {
            return Ok(Self::root());
        }
        if !input.starts_with('/') {
            return Err(XPathError(input.to_string()));
        }
        let mut steps = Vec::new();
        for segment in input[1..].split('/') {
            if segment.is_empty() {
                return Err(XPathError(input.to_string()));
            }
            let (name, instance) = split_instance(segment)?;
            if name.is_empty() || !instance_well_formed(segment) {
                return Err(XPathError(input.to_string()));
            }
            steps.push(Step::Field(name.to_string()));
            if let Some(index) = instance {
                steps.push(Step::Index(index));
            }
        }
        Ok(Self { steps })
    }

    /// Returns the steps.
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Whether this xpath is the root.
    pub fn is_root(&self) -> bool {
        self.steps.is_empty()
    }

    /// Splits off the first step, returning it and the remaining suffix.
    pub fn take_first(&self) -> Option<(Step, XPath)> {
        let (head, tail) = self.steps.split_first()?;
        Some((
            head.clone(),
            XPath {
                steps: tail.to_vec(),
            },
        ))
    }

    /// Appends a field step.
    pub fn field(mut self, name: impl Into<String>) -> Self {
        self.steps.push(Step::Field(name.into()));
        self
    }

    /// Appends an index step.
    pub fn index(mut self, index: usize) -> Self {
        self.steps.push(Step::Index(index));
        self
    }

    /// Joins this xpath with a suffix path (relative steps appended).
    pub fn join(&self, suffix: &XPath) -> XPath {
        let mut steps = self.steps.clone();
        steps.extend(suffix.steps.iter().cloned());
        XPath { steps }
    }
}

impl Display for XPath {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        if self.steps.is_empty() {
            return formatter.write_str("/");
        }
        for step in &self.steps {
            match step {
                Step::Field(name) => {
                    formatter.write_str("/")?;
                    formatter.write_str(name)?;
                }
                Step::Index(index) => write!(formatter, "[{index}]")?,
            }
        }
        Ok(())
    }
}

/// A parse failure message for the (stable) xpath grammar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XPathError(pub String);

impl std::fmt::Display for XPathError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid xpath '{}'", self.0)
    }
}

impl std::error::Error for XPathError {}

/// Splits a segment like `b[3]` into (`b`, `[3]`). Bare `b` yields
/// (`b`, `None`). A bracketed suffix whose content is not a base-10 index is a
/// parse error — the `[key]` spelling no longer exists.
fn split_instance(segment: &str) -> Result<(&str, Option<usize>), XPathError> {
    let open = segment.find('[');
    let Some(open) = open else {
        return Ok((segment, None));
    };
    if !segment.ends_with(']') {
        return Ok((segment, None));
    }
    let inner = &segment[open + 1..segment.len() - 1];
    let name = &segment[..open];
    if inner.is_empty() {
        return Ok((name, None));
    }
    let index = inner
        .parse::<usize>()
        .map_err(|_| XPathError(segment.to_string()))?;
    Ok((name, Some(index)))
}

/// A segment is malformed when it contains an unclosed or double bracket
/// (for example `b[` or `b[3][4]`), which must be rejected rather than
/// silently treated as a field name containing `[`.
fn instance_well_formed(segment: &str) -> bool {
    let open = segment.matches('[').count();
    if open == 0 {
        return true;
    }
    segment.ends_with(']') && open == segment.matches(']').count() && open == 1
}
