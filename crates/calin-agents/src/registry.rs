use std::sync::Arc;
use std::time::Duration;

use crate::mock::{MockConfig, MockRunner};
use crate::runner::Runner;

/// Set to `1` to run every agent on the mock runner.
const ENV_DRY_RUN: &str = "CALIN_DRY_RUN";
/// Milliseconds the mock runner waits before each chunk.
const ENV_MOCK_DELAY_MS: &str = "CALIN_MOCK_DELAY_MS";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegistryConfig {
    /// Resolve every runner id to the mock runner.
    pub dry_run: bool,
    pub mock: MockConfig,
}

impl RegistryConfig {
    /// Reads `CALIN_DRY_RUN` and `CALIN_MOCK_DELAY_MS`. Only the binary calls
    /// this; everything else receives a config, so tests never touch the
    /// process environment.
    pub fn from_env() -> Self {
        Self::from_values(
            std::env::var(ENV_DRY_RUN).ok().as_deref(),
            std::env::var(ENV_MOCK_DELAY_MS).ok().as_deref(),
        )
    }

    fn from_values(dry_run: Option<&str>, mock_delay_ms: Option<&str>) -> Self {
        let mut config = Self {
            dry_run: dry_run.is_some_and(|value| value.trim() == "1"),
            ..Self::default()
        };
        // An unparseable delay keeps the default: every setting must have a working value.
        if let Some(ms) = mock_delay_ms.and_then(|value| value.trim().parse().ok()) {
            config.mock.delay = Duration::from_millis(ms);
        }
        config
    }
}

/// The runners this server can start. The first one is the default.
#[derive(Clone)]
pub struct RunnerRegistry {
    dry_run: bool,
    mock: Arc<dyn Runner>,
    runners: Vec<Arc<dyn Runner>>,
}

impl RunnerRegistry {
    /// A registry holding the mock runner, which is the default until real
    /// runners are registered ahead of it.
    pub fn new(config: RegistryConfig) -> Self {
        let mock: Arc<dyn Runner> = Arc::new(MockRunner::new(config.mock));
        Self {
            dry_run: config.dry_run,
            runners: vec![Arc::clone(&mock)],
            mock,
        }
    }

    /// Adds a runner after the existing ones, replacing one with the same id.
    pub fn register(&mut self, runner: Arc<dyn Runner>) {
        match self.runners.iter_mut().find(|r| r.id() == runner.id()) {
            Some(existing) => *existing = runner,
            None => self.runners.push(runner),
        }
    }

    pub fn dry_run(&self) -> bool {
        self.dry_run
    }

    /// Runner ids, the default one first.
    pub fn ids(&self) -> Vec<String> {
        self.runners.iter().map(|r| r.id().to_owned()).collect()
    }

    /// The runner for `id`, or the default runner when `id` is absent.
    /// `None` when the id is unknown. In dry run every id is the mock runner.
    pub fn resolve(&self, id: Option<&str>) -> Option<Arc<dyn Runner>> {
        if self.dry_run {
            return Some(Arc::clone(&self.mock));
        }
        match id {
            Some(id) => self.runners.iter().find(|r| r.id() == id).cloned(),
            None => self.runners.first().cloned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MOCK_RUNNER_ID;
    use crate::runner::{AgentHandle, RunnerError, StartParams};

    struct Other;

    impl Runner for Other {
        fn id(&self) -> &str {
            "other"
        }

        fn start(&self, _params: StartParams) -> Result<AgentHandle, RunnerError> {
            Err(RunnerError::Start("not a real runner".into()))
        }
    }

    #[test]
    fn mock_is_the_default_and_unknown_ids_do_not_resolve() {
        let registry = RunnerRegistry::new(RegistryConfig::default());
        assert!(!registry.dry_run());
        assert_eq!(registry.ids(), [MOCK_RUNNER_ID]);
        assert_eq!(registry.resolve(None).unwrap().id(), MOCK_RUNNER_ID);
        assert_eq!(registry.resolve(Some("mock")).unwrap().id(), MOCK_RUNNER_ID);
        assert!(registry.resolve(Some("claude")).is_none());
    }

    #[test]
    fn registered_runners_resolve_by_id() {
        let mut registry = RunnerRegistry::new(RegistryConfig::default());
        registry.register(Arc::new(Other));
        registry.register(Arc::new(Other));
        assert_eq!(registry.ids(), [MOCK_RUNNER_ID, "other"]);
        assert_eq!(registry.resolve(Some("other")).unwrap().id(), "other");
    }

    #[test]
    fn dry_run_resolves_every_id_to_the_mock_runner() {
        let mut registry = RunnerRegistry::new(RegistryConfig {
            dry_run: true,
            ..RegistryConfig::default()
        });
        registry.register(Arc::new(Other));
        for id in [None, Some("mock"), Some("other"), Some("claude")] {
            assert_eq!(registry.resolve(id).unwrap().id(), MOCK_RUNNER_ID);
        }
    }

    #[test]
    fn config_is_read_from_environment_values() {
        assert_eq!(
            RegistryConfig::from_values(None, None),
            RegistryConfig::default()
        );
        let config = RegistryConfig::from_values(Some("1"), Some("0"));
        assert!(config.dry_run);
        assert_eq!(config.mock.delay, Duration::ZERO);
        let config = RegistryConfig::from_values(Some("true"), Some("soon"));
        assert!(!config.dry_run);
        assert_eq!(config.mock.delay, MockConfig::default().delay);
    }
}
