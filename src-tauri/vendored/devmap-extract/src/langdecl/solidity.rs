//! Solidity declarations.
//!
//! The shared node-kind table already covers most of Solidity: a contract, a
//! library, an interface, a struct, an enum and a function all reach the symbol
//! table through it, and measured on a fixture with two contract functions the
//! extractor emitted all four expected symbols. This module exists for the one
//! declaration the shared table has no name for and that Solidity code cannot
//! be read without.
//!
//! A **modifier** is a callable body wrapped around a function — `onlyOwner`,
//! `nonReentrant`, `whenNotPaused` — and it is where a contract's access
//! control and reentrancy protection live. `modifier_definition` is in no shared
//! table, so before this module a modifier was not a node at all: `dev map
//! impact` on it returned nothing, `dev map dead` could not see it, and the
//! `modifier_invocation` edges `langcalls::solidity` records would have named a
//! callee no symbol carried.
//!
//! Everything else is delegated to [`super::generic`] rather than restated, so
//! this module cannot fall behind the shared table for the eight kinds it does
//! not answer.
//!
//! # Events and errors are declarations, not callables
//!
//! `emit Transfer(…)` and `revert Unauthorized(…)` look like calls and are
//! deliberately not recorded as such, here or in `langcalls::solidity`. An
//! `event` and an `error` declare a named tuple of typed fields and have no
//! body: there is nothing for a call edge to reach, and `impact` through one
//! would traverse into an empty node. Both still reach the graph as `Name`
//! references attributed to the function that emits them, which is where the
//! generic identifier walk already puts them — verified on the fixture:
//! `REF Name name=Deposit encl=Some("Vault.sol::Vault.deposit")`.

use tree_sitter::Node;

use crate::model::SymbolKind;
use crate::treesitter::{get_child_text, is_callee_identity};

use super::Declaration;

/// A Solidity declaration, or `None` when `node` declares nothing this graph
/// records.
pub(crate) fn declaration(node: Node, source: &str) -> Option<Declaration> {
    if node.kind() != "modifier_definition" {
        return super::generic(node, source);
    }
    let name = get_child_text(node, "name", source).filter(|name| is_callee_identity(name))?;
    // A modifier is always declared inside the contract or library that owns
    // it, and the owner comes from the same helper every other kind here uses,
    // so a modifier and a function of one contract are qualified identically.
    Declaration::new(
        SymbolKind::Function,
        crate::treesitter::generic_enclosing_type(node, source),
        name,
    )
}
