//! Tests across modules: parsing and validation, the engine, and a seeded property test.

mod engine;
mod parsing;
mod property;

use crate::{Problems, Workflow, parse};

/// The example file of spec section 2, read from the spec itself so it stays verbatim.
pub(crate) fn spec_example() -> &'static str {
    let spec = include_str!("../../../../docs/specs/2026-11-27-workflows.md");
    let section = &spec[spec.find("## 2. File format").expect("section 2")..];
    let start = section.find("```yaml\n").expect("yaml block") + "```yaml\n".len();
    let len = section[start..].find("```").expect("end of block");
    &section[start..start + len]
}

pub(crate) fn ok(yaml: &str) -> Workflow {
    match parse("test", yaml) {
        Ok(w) => w,
        Err(p) => panic!("expected a valid workflow, got:\n{p}\n---\n{yaml}"),
    }
}

pub(crate) fn problems(yaml: &str) -> Problems {
    match parse("test", yaml) {
        Ok(_) => panic!("expected problems for:\n{yaml}"),
        Err(p) => {
            assert!(!p.is_empty());
            p
        }
    }
}

/// Wraps node and edge lines (already indented as list items) into a whole document.
pub(crate) fn doc(nodes: &str, edges: &str) -> String {
    format!("version: 1\nname: Test\nnodes:\n{nodes}edges:\n{edges}")
}
