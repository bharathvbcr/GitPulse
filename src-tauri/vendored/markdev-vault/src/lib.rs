//! Vault indexing: notes, the link graph, tags, and search.

pub mod graph;
pub mod index;
pub mod note;
pub mod rename;
pub mod search;

pub use graph::{Graph, GraphEdge, GraphNode, GraphQuery};
pub use index::{
    Backlink, OutgoingLink, Resolution, SearchHit, TagCount, UnlinkedMention, Vault,
    VaultScanLimits, VaultScanStatus, DEFAULT_MAX_NOTE_BYTES, DEFAULT_MAX_VAULT_BYTES,
};
pub use note::{Heading, Note, NoteLinkKind, WikiLink};
