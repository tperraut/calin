//! The workflows a session can run: the built-in `quick` and the files in
//! the home repo's `.calin/workflows/` (workflows spec, section 1). Files are
//! parsed by `calin-workflow`; this module finds them, remembers what they
//! held and turns them into the shapes a client sees.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use calin_proto::{WorkflowEdge, WorkflowGraph, WorkflowInfo, WorkflowNodeInfo, WorkflowPort};
use calin_workflow::{NodeKind, Workflow};

use crate::error::{Error, Result};
use crate::repos::blocking;

/// Where the files are, inside the home repo.
pub(crate) const WORKFLOWS_DIR: &str = ".calin/workflows";
/// The id of the built-in workflow, and of a session that names none.
pub(crate) const QUICK: &str = "quick";
/// A workflow is a page of YAML; anything larger is not one.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// One workflow of the home repo: usable, or not and why.
#[derive(Clone, Debug)]
enum Loaded {
    Valid(Box<Workflow>),
    Invalid { id: String, problem: String },
}

impl Loaded {
    fn id(&self) -> &str {
        match self {
            Self::Valid(workflow) => workflow.id(),
            Self::Invalid { id, .. } => id,
        }
    }
}

/// What a workflow file looked like when it was read: enough to tell that
/// it changed without reading it again.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    name: String,
    modified: Option<SystemTime>,
    len: u64,
}

#[derive(Default)]
struct State {
    /// The home repo of the settings in effect.
    home: Option<PathBuf>,
    /// The directory's listing at the last read; `None` forces a read.
    stamps: Option<Vec<Stamp>>,
    /// By id.
    loaded: Vec<Loaded>,
}

/// The home repo's workflows, read again only when their files changed.
#[derive(Default)]
pub(crate) struct WorkflowCatalog {
    /// Held across a read of the directory, so two requests never parse the
    /// same files side by side.
    state: tokio::sync::Mutex<State>,
}

impl WorkflowCatalog {
    /// Points the catalog at the home repo of the settings in effect and
    /// forgets what was read: the next request reads the files again.
    pub(crate) async fn reload(&self, home_repo: Option<&str>) {
        let mut state = self.state.lock().await;
        state.home = home_repo.map(PathBuf::from);
        state.stamps = None;
        state.loaded.clear();
    }

    /// `quick` first, then the home repo's workflows by id.
    pub(crate) async fn list(&self) -> Vec<WorkflowInfo> {
        let mut all = vec![info(&calin_workflow::quick(), true)];
        all.extend(self.current().await.iter().map(|loaded| match loaded {
            Loaded::Valid(workflow) => info(workflow, false),
            Loaded::Invalid { id, problem } => WorkflowInfo {
                id: id.clone(),
                name: id.clone(),
                description: None,
                builtin: false,
                budget_usd: None,
                graph: None,
                problem: Some(problem.clone()),
            },
        }));
        all
    }

    /// The workflow a new session asks for: `None` for the built-in `quick`,
    /// which is also what an absent or blank id means. `Invalid` for an id
    /// nothing has and for a file that does not validate.
    pub(crate) async fn resolve(&self, id: Option<&str>) -> Result<Option<Workflow>> {
        let Some(id) = id
            .map(str::trim)
            .filter(|id| !id.is_empty() && *id != QUICK)
        else {
            return Ok(None);
        };
        let loaded = self.current().await;
        match loaded.iter().find(|loaded| loaded.id() == id) {
            Some(Loaded::Valid(workflow)) => Ok(Some((**workflow).clone())),
            Some(Loaded::Invalid { problem, .. }) => Err(Error::Invalid(format!(
                "the workflow {id} cannot be used:\n{problem}"
            ))),
            None => Err(Error::Invalid(format!("unknown workflow {id}"))),
        }
    }

    /// What the files hold now. Lists the directory every time, and parses
    /// only when the listing differs from the one of the last read.
    async fn current(&self) -> Vec<Loaded> {
        let mut state = self.state.lock().await;
        let Some(home) = state.home.clone() else {
            return Vec::new();
        };
        let known = state.stamps.clone();
        let read = blocking(move || {
            let dir = home.join(WORKFLOWS_DIR);
            let stamps = stamps(&dir);
            if known.as_ref() == Some(&stamps) {
                return Ok(None);
            }
            let loaded = load(&dir, &stamps);
            Ok(Some((stamps, loaded)))
        });
        match read.await {
            Ok(Some((stamps, loaded))) => {
                state.stamps = Some(stamps);
                state.loaded = loaded;
            }
            Ok(None) => {}
            // The read itself fails nothing; this is the thread it ran on.
            Err(err) => tracing::warn!(%err, "could not read the home repo's workflows"),
        }
        state.loaded.clone()
    }
}

/// Blocking. The workflow files of `dir` by name. A directory that is not
/// there, or cannot be listed, holds none.
fn stamps(dir: &Path) -> Vec<Stamp> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut stamps: Vec<Stamp> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            file_id(&name)?;
            // Follows links: a workflow may be a link to a shared file.
            let meta = std::fs::metadata(entry.path()).ok()?;
            meta.is_file().then(|| Stamp {
                name,
                modified: meta.modified().ok(),
                len: meta.len(),
            })
        })
        .collect();
    stamps.sort_by(|a, b| a.name.cmp(&b.name));
    stamps
}

/// The id a file name gives its workflow: the name without `.yaml` or
/// `.yml`. `None` for any other file.
fn file_id(name: &str) -> Option<&str> {
    let id = name
        .strip_suffix(".yaml")
        .or_else(|| name.strip_suffix(".yml"))?;
    (!id.is_empty()).then_some(id)
}

/// Blocking. Reads and parses every listed file. Nothing here fails: what
/// is wrong with a file becomes its problem.
fn load(dir: &Path, stamps: &[Stamp]) -> Vec<Loaded> {
    let mut loaded: Vec<Loaded> = Vec::with_capacity(stamps.len());
    let mut files: Vec<(&str, &str)> = Vec::with_capacity(stamps.len());
    for stamp in stamps {
        let Some(id) = file_id(&stamp.name) else {
            continue;
        };
        // `a.yaml` and `a.yml` would be one id: neither can be meant.
        if let Some((_, other)) = files.iter().find(|(known, _)| *known == id) {
            let problem = format!(
                "both {other} and {} define the workflow {id}; keep one of them",
                stamp.name
            );
            loaded.retain(|known| known.id() != id);
            loaded.push(Loaded::Invalid {
                id: id.to_owned(),
                problem,
            });
            continue;
        }
        files.push((id, &stamp.name));
        let invalid = |problem: String| Loaded::Invalid {
            id: id.to_owned(),
            problem,
        };
        if stamp.len > MAX_FILE_BYTES {
            loaded.push(invalid(format!(
                "the file is larger than {MAX_FILE_BYTES} bytes, which no workflow is"
            )));
            continue;
        }
        loaded.push(match std::fs::read_to_string(dir.join(&stamp.name)) {
            Ok(yaml) => match calin_workflow::parse(id, &yaml) {
                Ok(workflow) => Loaded::Valid(Box::new(workflow)),
                // Every problem, one per line.
                Err(problems) => invalid(problems.to_string()),
            },
            Err(err) => invalid(format!("the file cannot be read: {err}")),
        });
    }
    loaded.sort_by(|a, b| a.id().cmp(b.id()));
    loaded
}

fn info(workflow: &Workflow, builtin: bool) -> WorkflowInfo {
    WorkflowInfo {
        id: workflow.id().to_owned(),
        name: workflow.name().to_owned(),
        description: workflow.description().map(str::to_owned),
        builtin,
        budget_usd: workflow.budget_usd(),
        graph: Some(graph(workflow)),
        problem: None,
    }
}

/// The workflow as a client draws it: nodes and edges in file order.
pub(crate) fn graph(workflow: &Workflow) -> WorkflowGraph {
    let nodes = workflow.nodes().iter().map(|node| WorkflowNodeInfo {
        id: node.id.clone(),
        kind: node.node_type().as_str().to_owned(),
        label: match node.kind {
            // Written here, where `loop_max` reads it back: the contract
            // has no other place for a loop's limit.
            NodeKind::Loop { max } => loop_label(max),
            _ => node.label(),
        },
        ports: node
            .ports()
            .into_iter()
            .map(|port| WorkflowPort {
                name: port.name,
                failure: port.failure,
            })
            .collect(),
    });
    let edges = workflow.edges().iter().map(|edge| WorkflowEdge {
        from: edge.from.clone(),
        port: edge.port.clone(),
        to: edge.to.clone(),
    });
    WorkflowGraph {
        nodes: nodes.collect(),
        edges: edges.collect(),
    }
}

const LOOP_LABEL: (&str, &str) = ("Up to ", " times");

/// The label of a loop node, which is where its limit is recorded.
fn loop_label(max: u32) -> String {
    format!("{}{max}{}", LOOP_LABEL.0, LOOP_LABEL.1)
}

/// The limit of a loop node of a recorded graph, read back from its label.
pub(crate) fn loop_max(node: &WorkflowNodeInfo) -> Option<u32> {
    if node.kind != "loop" {
        return None;
    }
    let count = node.label.strip_prefix(LOOP_LABEL.0)?;
    count.strip_suffix(LOOP_LABEL.1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::testing::{git_repo, write_file};

    const VALID: &str = r#"
version: 1
name: Implement and test
description: The usual round.
budget_usd: 2.5
nodes:
  - { id: start, type: start }
  - { id: implement, type: agent, prompt: "{{task}}" }
  - { id: tests, type: check, command: "make test" }
  - { id: retry, type: loop, max: 3 }
  - { id: review, type: agent, prompt: "Review it.", verdicts: [approve, reject] }
  - { id: ok, type: end }
edges:
  - { from: start, to: implement }
  - { from: implement.done, to: tests }
  - { from: tests.pass, to: review }
  - { from: tests.fail, to: retry }
  - { from: retry.repeat, to: implement }
  - { from: review.approve, to: ok }
"#;

    /// A home repo with these workflow files, and a catalog pointed at it.
    async fn catalog(files: &[(&str, &str)]) -> (TempDir, PathBuf, WorkflowCatalog) {
        let dir = TempDir::new().unwrap();
        let home = git_repo(dir.path(), "home");
        for (name, yaml) in files {
            write_file(&home, &format!("{WORKFLOWS_DIR}/{name}"), yaml);
        }
        let catalog = WorkflowCatalog::default();
        catalog.reload(home.to_str()).await;
        (dir, home, catalog)
    }

    fn ids(list: &[WorkflowInfo]) -> Vec<&str> {
        list.iter().map(|info| info.id.as_str()).collect()
    }

    #[tokio::test]
    async fn without_a_home_repo_there_is_only_quick() {
        let catalog = WorkflowCatalog::default();
        let list = catalog.list().await;
        assert_eq!(ids(&list), ["quick"]);
        let quick = &list[0];
        assert!(quick.builtin);
        assert_eq!(quick.name, "Quick");
        assert_eq!(quick.problem, None);
        assert_eq!(quick.budget_usd, None);
        let graph = quick.graph.as_ref().unwrap();
        let kinds: Vec<&str> = graph.nodes.iter().map(|node| node.kind.as_str()).collect();
        assert_eq!(kinds, ["start", "agent", "end"]);
        assert_eq!(graph.edges.len(), 2);

        // `quick`, absent and blank all mean the built-in behavior.
        for id in [None, Some("quick"), Some(""), Some("  ")] {
            assert!(catalog.resolve(id).await.unwrap().is_none(), "{id:?}");
        }
        let unknown = catalog.resolve(Some("implement")).await;
        assert!(matches!(unknown, Err(Error::Invalid(_))), "{unknown:?}");

        // A home repo without the directory has none either.
        let (_dir, _home, catalog) = super::tests::catalog(&[]).await;
        assert_eq!(ids(&catalog.list().await), ["quick"]);
    }

    #[tokio::test]
    async fn files_are_listed_by_id_with_their_graph_or_their_problems() {
        let broken = "version: 2\nname: Broken\nnodes: []\nedges: []\n";
        let (_dir, home, catalog) = super::tests::catalog(&[
            ("zeta.yml", VALID),
            ("implement.yaml", VALID),
            ("broken.yaml", broken),
            ("notes.txt", "not a workflow"),
            ("README.md", "# Workflows"),
        ])
        .await;
        std::fs::create_dir(home.join(WORKFLOWS_DIR).join("folder.yaml")).unwrap();

        let list = catalog.list().await;
        assert_eq!(ids(&list), ["quick", "broken", "implement", "zeta"]);

        let implement = &list[2];
        assert!(!implement.builtin);
        assert_eq!(implement.name, "Implement and test");
        assert_eq!(implement.description.as_deref(), Some("The usual round."));
        assert_eq!(implement.budget_usd, Some(2.5));
        assert_eq!(implement.problem, None);
        // The same file under `.yml` is a workflow of its own.
        assert_eq!(list[3].graph, implement.graph);

        let graph = implement.graph.as_ref().unwrap();
        let node = |id: &str| graph.nodes.iter().find(|node| node.id == id).unwrap();
        let order: Vec<&str> = graph.nodes.iter().map(|node| node.id.as_str()).collect();
        assert_eq!(
            order,
            ["start", "implement", "tests", "retry", "review", "ok"]
        );
        assert_eq!(node("implement").kind, "agent");
        assert_eq!(node("implement").label, "{{task}}");
        assert_eq!(node("tests").label, "make test");
        let ports = |id: &str| -> Vec<(&str, bool)> {
            let ports = node(id).ports.iter();
            ports
                .map(|port| (port.name.as_str(), port.failure))
                .collect()
        };
        assert_eq!(ports("start"), [("next", false)]);
        assert_eq!(ports("implement"), [("done", false), ("failed", true)]);
        assert_eq!(ports("tests"), [("pass", false), ("fail", true)]);
        assert_eq!(ports("retry"), [("repeat", false), ("exhausted", true)]);
        // A verdict called `reject` is a failure port by its name.
        assert_eq!(
            ports("review"),
            [("approve", false), ("reject", true), ("failed", true)]
        );
        assert!(ports("ok").is_empty());
        // A loop's limit is in its label, and reads back from there.
        assert_eq!(node("retry").label, "Up to 3 times");
        assert_eq!(loop_max(node("retry")), Some(3));
        assert_eq!(loop_max(node("tests")), None);
        assert_eq!(graph.edges.len(), 6);
        assert_eq!(
            graph.edges[0],
            WorkflowEdge {
                from: "start".into(),
                port: "next".into(),
                to: "implement".into(),
            }
        );
        assert_eq!(graph.edges[4].port, "repeat");

        let broken = &list[1];
        assert_eq!(broken.name, "broken");
        assert_eq!(broken.graph, None);
        let problem = broken.problem.as_deref().unwrap();
        // All of them, one per line.
        assert!(problem.lines().count() >= 2, "{problem}");
        assert!(problem.contains("version"), "{problem}");

        assert!(catalog.resolve(Some("implement")).await.unwrap().is_some());
        assert_eq!(
            catalog.resolve(Some(" zeta ")).await.unwrap().unwrap().id(),
            "zeta"
        );
        let refused = catalog.resolve(Some("broken")).await.unwrap_err();
        assert!(matches!(&refused, Error::Invalid(why) if why.contains("version")));
        let refused = catalog.resolve(Some("notes")).await;
        assert!(matches!(refused, Err(Error::Invalid(_))));
    }

    #[tokio::test]
    async fn an_edited_file_shows_without_a_reload_and_an_unchanged_one_is_not_parsed_again() {
        let (_dir, home, catalog) = super::tests::catalog(&[("implement.yaml", VALID)]).await;
        assert_eq!(catalog.list().await[1].name, "Implement and test");
        let read = catalog.state.lock().await.stamps.clone();
        assert_eq!(read.as_ref().map(Vec::len), Some(1));

        // Nothing changed: the listing is the one of the last read.
        catalog.list().await;
        assert_eq!(catalog.state.lock().await.stamps, read);

        // Edited (another length, whatever the clock's resolution).
        let renamed = VALID.replace("Implement and test", "Implement, test, repeat");
        write_file(&home, ".calin/workflows/implement.yaml", &renamed);
        assert_eq!(catalog.list().await[1].name, "Implement, test, repeat");

        // Broken, then mended; added, then removed.
        write_file(&home, ".calin/workflows/implement.yaml", "name: [");
        let list = catalog.list().await;
        assert!(list[1].problem.is_some() && list[1].graph.is_none());
        let refused = catalog.resolve(Some("implement")).await;
        assert!(matches!(refused, Err(Error::Invalid(_))));
        write_file(&home, ".calin/workflows/implement.yaml", VALID);
        write_file(&home, ".calin/workflows/second.yml", VALID);
        assert_eq!(ids(&catalog.list().await), ["quick", "implement", "second"]);
        std::fs::remove_file(home.join(".calin/workflows/second.yml")).unwrap();
        assert_eq!(ids(&catalog.list().await), ["quick", "implement"]);
        assert!(catalog.resolve(Some("second")).await.is_err());

        // Another home repo, or none: what was read is forgotten.
        catalog.reload(None).await;
        assert_eq!(ids(&catalog.list().await), ["quick"]);
    }

    #[tokio::test]
    async fn two_files_with_one_id_and_ids_that_cannot_be_are_listed_as_problems() {
        let (_dir, _home, catalog) = super::tests::catalog(&[
            ("twice.yaml", VALID),
            ("twice.yml", VALID),
            ("Not An Id.yaml", VALID),
            ("quick.yaml", VALID),
        ])
        .await;
        let list = catalog.list().await;
        assert_eq!(ids(&list), ["quick", "Not An Id", "quick", "twice"]);
        assert!(list[0].builtin && list[0].graph.is_some());
        for unusable in &list[1..] {
            assert!(!unusable.builtin);
            assert!(unusable.graph.is_none(), "{}", unusable.id);
            assert!(unusable.problem.is_some(), "{}", unusable.id);
        }
        let twice = list[3].problem.as_deref().unwrap();
        assert!(twice.contains("twice.yaml") && twice.contains("twice.yml"));
        assert!(catalog.resolve(Some("twice")).await.is_err());
        // The name always means the built-in one.
        assert!(catalog.resolve(Some("quick")).await.unwrap().is_none());
    }
}
