//! The tokens that let an agent reach Calin's tool endpoint. One per agent
//! start, held in memory only, gone when the agent is.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

/// 256 bits from the operating system, far past guessing.
const TOKEN_BYTES: usize = 32;

/// The agent a token speaks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRef {
    pub session_id: String,
    pub agent_id: String,
}

#[derive(Default)]
pub(crate) struct Tokens {
    agents: Mutex<HashMap<String, AgentRef>>,
}

/// An issued token. Dropping it revokes the token, so no path that ends an
/// agent can leave its token behind. Deliberately not `Debug`: a token must
/// never reach a log.
pub(crate) struct Token {
    value: String,
    tokens: Arc<Tokens>,
}

impl Token {
    pub(crate) fn value(&self) -> &str {
        &self.value
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        self.tokens.lock().remove(&self.value);
    }
}

impl Tokens {
    /// A new token for the agent, or `None` when the system has no
    /// randomness to give: the agent then runs without Calin's tools.
    pub(crate) fn issue(self: &Arc<Self>, agent: AgentRef) -> Option<Token> {
        let mut bytes = [0u8; TOKEN_BYTES];
        if let Err(err) = getrandom::fill(&mut bytes) {
            tracing::error!(%err, "no randomness for an agent token");
            return None;
        }
        let value: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        self.lock().insert(value.clone(), agent);
        Some(Token {
            value,
            tokens: Arc::clone(self),
        })
    }

    pub(crate) fn agent(&self, token: &str) -> Option<AgentRef> {
        self.lock().get(token).cloned()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, AgentRef>> {
        // Every critical section is one insert, remove or lookup.
        self.agents.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str) -> AgentRef {
        AgentRef {
            session_id: "s1".to_owned(),
            agent_id: id.to_owned(),
        }
    }

    #[test]
    fn a_token_names_its_agent_until_it_is_dropped() {
        let tokens = Arc::new(Tokens::default());
        let first = tokens.issue(agent("a1")).unwrap();
        let second = tokens.issue(agent("a2")).unwrap();
        assert_eq!(first.value().len(), 2 * TOKEN_BYTES);
        assert!(first.value().bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(first.value(), second.value());
        assert_eq!(tokens.agent(first.value()), Some(agent("a1")));
        assert_eq!(tokens.agent(second.value()), Some(agent("a2")));
        assert_eq!(tokens.agent(""), None);
        assert_eq!(tokens.agent(&first.value()[1..]), None);

        let revoked = first.value().to_owned();
        drop(first);
        assert_eq!(tokens.agent(&revoked), None);
        assert_eq!(tokens.agent(second.value()), Some(agent("a2")));
    }

    #[test]
    fn two_starts_of_one_agent_have_two_tokens() {
        let tokens = Arc::new(Tokens::default());
        let old = tokens.issue(agent("a1")).unwrap();
        let new = tokens.issue(agent("a1")).unwrap();
        assert_ne!(old.value(), new.value());
        drop(old);
        assert_eq!(tokens.agent(new.value()), Some(agent("a1")));
    }
}
