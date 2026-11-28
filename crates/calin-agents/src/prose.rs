//! Which kind of prose item a runner is streaming: what the agent says, or
//! what it thinks.

use calin_proto::{ItemContent, ItemDelta};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Prose {
    Text,
    Reasoning,
}

impl Prose {
    pub(crate) fn content(self, text: String) -> ItemContent {
        match self {
            Self::Text => ItemContent::Text { text },
            Self::Reasoning => ItemContent::Reasoning { text },
        }
    }

    pub(crate) fn delta(self, text: String) -> ItemDelta {
        match self {
            Self::Text => ItemDelta::Text { text },
            Self::Reasoning => ItemDelta::Reasoning { text },
        }
    }
}
