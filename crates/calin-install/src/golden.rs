//! Golden files for everything `install` and `uninstall` print or write.
//! `CALIN_UPDATE_GOLDEN=1 cargo test -p calin-install` rewrites them.

use std::path::{Path, PathBuf};

use crate::input::InstallInputBuilder;
use crate::plan::{install_plan, render_steps, uninstall_plan};
use crate::render::tests::{awkward, typical};
use crate::render::{render_caddy, render_env, render_unit};

const STAGING: &str = "/root/calin-staging";

fn golden_dir(case: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("golden")
        .join(case)
}

/// `lcs[i][j]`: the length of the longest common subsequence of `a[i..]` and `b[j..]`.
fn lcs_table(a: &[&str], b: &[&str]) -> Vec<Vec<usize>> {
    let mut lcs = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    lcs
}

/// A line diff (longest common subsequence), `-` expected, `+` actual.
fn diff(expected: &str, actual: &str) -> String {
    let a: Vec<&str> = expected.lines().collect();
    let b: Vec<&str> = actual.lines().collect();
    let lcs = lcs_table(&a, &b);
    let (mut i, mut j, mut out) = (0, 0, String::new());
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            out.push_str(&format!("  {}\n", a[i]));
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || lcs[i][j + 1] >= lcs[i + 1][j]) {
            out.push_str(&format!("+ {}\n", b[j]));
            j += 1;
        } else {
            out.push_str(&format!("- {}\n", a[i]));
            i += 1;
        }
    }
    out
}

fn compare(case: &str, name: &str, actual: &str) -> Option<String> {
    let path = golden_dir(case).join(name);
    if std::env::var("CALIN_UPDATE_GOLDEN").as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return None;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    (expected != actual).then(|| {
        format!(
            "{} differs from its golden file (CALIN_UPDATE_GOLDEN=1 rewrites it):\n{}",
            path.display(),
            diff(&expected, actual)
        )
    })
}

fn check_case(case: &str, builder: InstallInputBuilder) {
    let input = builder.build().unwrap();
    let plan = install_plan(&input, Path::new(STAGING)).unwrap();
    let outputs = [
        ("calin.service", render_unit(input.layout())),
        ("calin.env", render_env(&input)),
        ("calin.caddy", render_caddy(input.domain(), input.layout())),
        ("install.txt", render_steps(&plan.steps)),
        ("uninstall.txt", uninstall_plan(&input).to_string()),
    ];
    // The plan's files are the rendered ones.
    for file in &plan.files {
        let rendered = &outputs
            .iter()
            .find(|(name, _)| *name == file.staging_name)
            .unwrap()
            .1;
        assert_eq!(&file.content, rendered);
    }
    let failures: Vec<String> = outputs
        .iter()
        .filter_map(|(name, actual)| compare(case, name, actual))
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn golden_typical() {
    check_case("typical", typical());
}

#[test]
fn golden_awkward() {
    check_case("awkward", awkward());
}

#[test]
fn the_diff_marks_changes() {
    assert_eq!(diff("a\nb\nc\n", "a\nx\nc\n"), "  a\n+ x\n- b\n  c\n");
}
