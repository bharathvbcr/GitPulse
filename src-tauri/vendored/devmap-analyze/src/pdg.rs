//! Structured intra-procedural control and data dependency graph.
//!
//! The builder accepts a grammar-owned statement tree. It does not infer
//! control flow from line numbers: branch, loop, exception, termination,
//! definitions, uses, and exact sink variables must be explicit at the
//! extraction boundary.
//!
//! # Bounds
//!
//! [`FunctionPdgInput`] derives `Deserialize`, so the statement tree is
//! untrusted input by construction and every walk over it is bounded:
//! [`MAX_PDG_NESTING_DEPTH`] on the recursion, [`MAX_PDG_STATEMENTS`] on the
//! breadth, and a pass ceiling on each of the two dataflow fixpoints. All four
//! are *refusals* — an `Err` naming the ceiling — never a truncated graph.
//! Returning a PDG built from part of a function would be a check that could
//! not run reporting as one that ran and passed, and the whole point of a data
//! dependency graph is that a caller trusts what it does not contain.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Deepest statement nesting the builder will walk.
///
/// Both walks over the tree — [`validate_statements`] and
/// `PdgBuilder::build_sequence` — recurse once per nesting level, and with no
/// cap the failure mode is `fatal runtime error: stack overflow` and a `SIGABRT`
/// that no caller can catch: a 100,000-level tree is a ~2 MB payload, far under
/// anything a transport would reject. Sixty-four levels is far past anything a
/// real function reaches (CPython's own compiler caps nesting at 20) and leaves
/// the deepest legal input using ~65 frames of the several thousand available.
pub const MAX_PDG_NESTING_DEPTH: usize = 64;

/// Most statements one function's PDG may hold, counting every nesting level.
///
/// Breadth is a ceiling for a different reason than depth: every statement and
/// every `elif` test becomes a CFG node (plus at most one exception dispatch
/// per `try`), and both fixpoints below sweep every node on every pass,
/// so the build is quadratic in this number. Measured on the debug profile with
/// a def-use chain — the worst shape — 800 statements took 1.3 s and the cost
/// grows with the square, which is why this sits at 2,000 and not higher. A
/// single function with more statements than this is not a function this
/// analysis has anything useful to say about.
pub const MAX_PDG_STATEMENTS: usize = 2_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CfgNodeKind {
    Entry,
    Exit,
    Block,
    Branch,
    Loop,
    Try,
    Return,
    Raise,
    /// Where an exception raised anywhere in a `try` body goes: to each
    /// handler, and — because a typed `except` may not match — onward through
    /// the `finally` or out of the function. One node per `try`, so the edge
    /// count stays linear in the body rather than body × handlers.
    ExceptionDispatch,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CfgNode {
    pub id: String,
    pub kind: CfgNodeKind,
    pub symbol: String,
    pub leader_line: u32,
    pub line_range: (u32, u32),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PdgEdge {
    pub source_node: String,
    pub target_node: String,
    pub edge_kind: String,
    pub variable: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FunctionPdg {
    pub function_name: String,
    pub generation_id: u32,
    pub content_hash: u64,
    pub nodes: BTreeMap<String, CfgNode>,
    pub edges: Vec<PdgEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FunctionPdgInput {
    pub function_name: String,
    pub generation_id: u32,
    pub content_hash: u64,
    pub start_line: u32,
    pub end_line: u32,
    pub params: Vec<String>,
    pub body: Vec<PdgStatement>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PdgStatement {
    pub line: u32,
    #[serde(default)]
    pub definitions: Vec<String>,
    #[serde(default)]
    pub uses: Vec<String>,
    /// Exact variable names consumed by a security-sensitive sink at this
    /// statement. Every sink must also appear in `uses`.
    #[serde(default)]
    pub taint_sinks: Vec<String>,
    pub kind: PdgStatementKind,
}

/// One `elif` of a [`PdgStatementKind::Branch`]: its condition's facts and the
/// body it guards.
///
/// An `elif` is a branch in the false arm of the one before it, but it is not
/// one nesting level deeper in the program, and modelling it as a nested
/// `Branch` made a 64-arm chain hit [`MAX_PDG_NESTING_DEPTH`] — the cap that
/// exists for hostile nesting, refusing ordinary dispatch code. The arms are
/// walked iteratively, so a chain costs statements against
/// [`MAX_PDG_STATEMENTS`], never depth.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ElifArm {
    pub line: u32,
    #[serde(default)]
    pub definitions: Vec<String>,
    #[serde(default)]
    pub uses: Vec<String>,
    /// Same contract as [`PdgStatement::taint_sinks`]: each is also a use.
    #[serde(default)]
    pub taint_sinks: Vec<String>,
    pub body: Vec<PdgStatement>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PdgStatementKind {
    Basic,
    /// `if`, its `elif` arms in source order, and the `else`. The false edge of
    /// each test enters the next arm's test; the last one's enters `else_body`.
    Branch {
        then_body: Vec<PdgStatement>,
        #[serde(default)]
        elif_arms: Vec<ElifArm>,
        else_body: Vec<PdgStatement>,
    },
    Loop {
        body: Vec<PdgStatement>,
    },
    Try {
        body: Vec<PdgStatement>,
        handlers: Vec<Vec<PdgStatement>>,
        finally_body: Vec<PdgStatement>,
    },
    Return,
    Raise,
}

impl FunctionPdgInput {
    /// Every `(line, variable)` a sink consumes, anywhere in the body, sorted
    /// by line and without duplicates.
    ///
    /// Walked with an explicit stack: the input is untrusted, and this is
    /// callable before [`build_function_pdg`] has validated its depth.
    pub fn taint_sinks(&self) -> Vec<(u32, String)> {
        let mut out = Vec::new();
        let mut pending: Vec<&[PdgStatement]> = vec![&self.body];
        while let Some(statements) = pending.pop() {
            for statement in statements {
                for sink in &statement.taint_sinks {
                    out.push((statement.line, sink.clone()));
                }
                match &statement.kind {
                    PdgStatementKind::Branch {
                        then_body,
                        elif_arms,
                        else_body,
                    } => {
                        pending.push(then_body);
                        for arm in elif_arms {
                            for sink in &arm.taint_sinks {
                                out.push((arm.line, sink.clone()));
                            }
                            pending.push(&arm.body);
                        }
                        pending.push(else_body);
                    }
                    PdgStatementKind::Loop { body } => pending.push(body),
                    PdgStatementKind::Try {
                        body,
                        handlers,
                        finally_body,
                    } => {
                        pending.push(body);
                        pending.extend(handlers.iter().map(Vec::as_slice));
                        pending.push(finally_body);
                    }
                    PdgStatementKind::Basic
                    | PdgStatementKind::Return
                    | PdgStatementKind::Raise => {}
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

#[derive(Debug, Clone, Default)]
struct NodeFacts {
    definitions: BTreeSet<String>,
    uses: BTreeSet<String>,
    sinks: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Incoming {
    node: String,
    edge_kind: &'static str,
}

/// Where control goes once a statement list is done with it.
#[derive(Debug, Default)]
struct Flow {
    /// Edges that fall through to whatever follows the list.
    normal: Vec<Incoming>,
    /// Edges that leave the function: a `return`, a `raise`, an exception no
    /// handler took. They are carried outward rather than wired to exit where
    /// they occur, because on the way out they run every `finally` that
    /// encloses them — wiring them straight to exit is what left a `finally`
    /// off every terminating path, and with no node at all when every path
    /// into it terminated.
    abrupt: Vec<Incoming>,
}

impl Flow {
    fn absorb(&mut self, other: Flow) {
        self.normal.extend(other.normal);
        self.abrupt.extend(other.abrupt);
    }

    /// Duplicate `(node, edge kind)` pairs add nothing but work: the edges
    /// they produce are deduplicated at the end anyway. Without this, a `try`
    /// with many empty handlers would hand the next statement the same
    /// predecessor once per handler.
    fn dedup(&mut self) {
        self.normal.sort();
        self.normal.dedup();
        self.abrupt.sort();
        self.abrupt.dedup();
    }
}

struct PdgBuilder<'a> {
    input: &'a FunctionPdgInput,
    nodes: BTreeMap<String, CfgNode>,
    facts: BTreeMap<String, NodeFacts>,
    edges: Vec<PdgEdge>,
    next_node: usize,
    /// Every statement node in creation order, so a `try` can name the nodes
    /// its body created — each one a point an exception can leave from.
    created: Vec<String>,
    entry: String,
    exit: String,
}

impl<'a> PdgBuilder<'a> {
    fn new(input: &'a FunctionPdgInput) -> Self {
        let entry = format!("{}:entry", input.function_name);
        let exit = format!("{}:exit", input.function_name);
        let mut builder = Self {
            input,
            nodes: BTreeMap::new(),
            facts: BTreeMap::new(),
            edges: Vec::new(),
            next_node: 0,
            created: Vec::new(),
            entry: entry.clone(),
            exit: exit.clone(),
        };
        builder.nodes.insert(
            entry.clone(),
            CfgNode {
                id: entry.clone(),
                kind: CfgNodeKind::Entry,
                symbol: input.function_name.clone(),
                leader_line: input.start_line,
                line_range: (input.start_line, input.start_line),
            },
        );
        builder.nodes.insert(
            exit.clone(),
            CfgNode {
                id: exit,
                kind: CfgNodeKind::Exit,
                symbol: input.function_name.clone(),
                leader_line: input.end_line,
                line_range: (input.end_line, input.end_line),
            },
        );
        builder.facts.insert(
            entry,
            NodeFacts {
                definitions: input.params.iter().cloned().collect(),
                ..NodeFacts::default()
            },
        );
        builder
    }

    fn add_control(&mut self, source: &str, target: &str, edge_kind: &str) {
        self.edges.push(PdgEdge {
            source_node: source.to_string(),
            target_node: target.to_string(),
            edge_kind: edge_kind.to_string(),
            variable: None,
        });
    }

    fn add_node(&mut self, line: u32, kind: CfgNodeKind, facts: NodeFacts) -> String {
        let id = format!("{}:{}:n{}", self.input.function_name, line, self.next_node);
        self.next_node += 1;
        self.nodes.insert(
            id.clone(),
            CfgNode {
                id: id.clone(),
                kind,
                symbol: self.input.function_name.clone(),
                leader_line: line,
                line_range: (line, line),
            },
        );
        self.facts.insert(id.clone(), facts);
        self.created.push(id.clone());
        id
    }

    fn add_statement_node(&mut self, statement: &PdgStatement) -> String {
        let kind = match statement.kind {
            PdgStatementKind::Basic => CfgNodeKind::Block,
            PdgStatementKind::Branch { .. } => CfgNodeKind::Branch,
            PdgStatementKind::Loop { .. } => CfgNodeKind::Loop,
            PdgStatementKind::Try { .. } => CfgNodeKind::Try,
            PdgStatementKind::Return => CfgNodeKind::Return,
            PdgStatementKind::Raise => CfgNodeKind::Raise,
        };
        let facts = node_facts(
            &statement.definitions,
            &statement.uses,
            &statement.taint_sinks,
        );
        self.add_node(statement.line, kind, facts)
    }

    /// Thread the CFG for one statement list.
    ///
    /// `depth` is the same nesting level [`validate_statements`] counted, and
    /// the cap is re-checked here rather than assumed: validation is the gate
    /// that makes exceeding it impossible, but a builder whose only protection
    /// lives in a function a future caller might skip is one refactor away from
    /// aborting the process again. Exceeding it is an error, never a shorter
    /// graph — a CFG missing the body of a loop is not a smaller answer, it is
    /// a wrong one.
    ///
    /// A statement after one every path out of which terminates gets no node:
    /// nothing reaches it. Terminating paths are returned in [`Flow::abrupt`]
    /// for the enclosing `try` or [`Self::finish`] to route.
    fn build_sequence(
        &mut self,
        statements: &[PdgStatement],
        incoming: Vec<Incoming>,
        depth: usize,
    ) -> anyhow::Result<Flow> {
        if depth > MAX_PDG_NESTING_DEPTH {
            anyhow::bail!(
                "PDG statement nesting exceeds the {MAX_PDG_NESTING_DEPTH}-level depth cap"
            );
        }
        let mut flow = Flow {
            normal: incoming,
            abrupt: Vec::new(),
        };
        for statement in statements {
            if flow.normal.is_empty() {
                break;
            }
            let node = self.add_statement_node(statement);
            for source in &flow.normal {
                self.add_control(&source.node, &node, source.edge_kind);
            }
            let inner = depth + 1;
            let after = match &statement.kind {
                PdgStatementKind::Basic => Flow {
                    normal: vec![Incoming {
                        node,
                        edge_kind: "control:fallthrough",
                    }],
                    abrupt: Vec::new(),
                },
                PdgStatementKind::Return | PdgStatementKind::Raise => Flow {
                    normal: Vec::new(),
                    abrupt: vec![Incoming {
                        node,
                        edge_kind: "control:terminate",
                    }],
                },
                PdgStatementKind::Branch {
                    then_body,
                    elif_arms,
                    else_body,
                } => {
                    let mut out = self.build_sequence(
                        then_body,
                        vec![Incoming {
                            node: node.clone(),
                            edge_kind: "control:true",
                        }],
                        inner,
                    )?;
                    // Each arm's test hangs off the false edge of the one
                    // before it, at this same depth: an `elif` chain is long,
                    // not deep.
                    let mut otherwise = Incoming {
                        node,
                        edge_kind: "control:false",
                    };
                    for arm in elif_arms {
                        let test = self.add_node(
                            arm.line,
                            CfgNodeKind::Branch,
                            node_facts(&arm.definitions, &arm.uses, &arm.taint_sinks),
                        );
                        self.add_control(&otherwise.node, &test, otherwise.edge_kind);
                        out.absorb(self.build_sequence(
                            &arm.body,
                            vec![Incoming {
                                node: test.clone(),
                                edge_kind: "control:true",
                            }],
                            inner,
                        )?);
                        otherwise = Incoming {
                            node: test,
                            edge_kind: "control:false",
                        };
                    }
                    out.absorb(self.build_sequence(else_body, vec![otherwise], inner)?);
                    out
                }
                PdgStatementKind::Loop { body } => {
                    let body_flow = self.build_sequence(
                        body,
                        vec![Incoming {
                            node: node.clone(),
                            edge_kind: "control:true",
                        }],
                        inner,
                    )?;
                    for tail in body_flow.normal {
                        self.add_control(&tail.node, &node, "control:loop");
                    }
                    Flow {
                        normal: vec![Incoming {
                            node,
                            edge_kind: "control:false",
                        }],
                        abrupt: body_flow.abrupt,
                    }
                }
                PdgStatementKind::Try {
                    body,
                    handlers,
                    finally_body,
                } => self.build_try(statement.line, node, body, handlers, finally_body, inner)?,
            };
            flow.normal = after.normal;
            flow.abrupt.extend(after.abrupt);
        }
        flow.dedup();
        Ok(flow)
    }

    /// `try` / `except` / `finally`.
    ///
    /// * The header enters the body, and enters each handler directly.
    /// * Every node the body creates — nested ones included — has an
    ///   exception edge to one [`CfgNodeKind::ExceptionDispatch`] node, which
    ///   enters each handler. That is what lets a definition made in the body
    ///   reach a use in a handler; a single edge from the header never could.
    /// * The dispatch also leaves abruptly, because a typed `except` may not
    ///   match.
    /// * The `finally` is entered by everything that leaves the body and the
    ///   handlers, normal and abrupt alike. Its tail continues normally if
    ///   anything entered normally, and continues outward (to an enclosing
    ///   `finally`, then exit) if anything entered abruptly.
    fn build_try(
        &mut self,
        line: u32,
        header: String,
        body: &[PdgStatement],
        handlers: &[Vec<PdgStatement>],
        finally_body: &[PdgStatement],
        inner: usize,
    ) -> anyhow::Result<Flow> {
        let first_body_node = self.created.len();
        let mut out = self.build_sequence(
            body,
            vec![Incoming {
                node: header.clone(),
                edge_kind: "control:try",
            }],
            inner,
        )?;
        if handlers.is_empty() && finally_body.is_empty() {
            return Ok(out);
        }
        let raisers = self.created[first_body_node..].to_vec();
        let dispatch = self.add_node(line, CfgNodeKind::ExceptionDispatch, NodeFacts::default());
        for raiser in &raisers {
            self.add_control(raiser, &dispatch, "control:exception");
        }
        for handler in handlers {
            out.absorb(self.build_sequence(
                handler,
                vec![
                    Incoming {
                        node: header.clone(),
                        edge_kind: "control:exception",
                    },
                    Incoming {
                        node: dispatch.clone(),
                        edge_kind: "control:exception",
                    },
                ],
                inner,
            )?);
        }
        out.abrupt.push(Incoming {
            node: dispatch,
            edge_kind: "control:unhandled",
        });
        if finally_body.is_empty() {
            return Ok(out);
        }
        let entered_normally = !out.normal.is_empty();
        let entered_abruptly = !out.abrupt.is_empty();
        let entry = out
            .normal
            .into_iter()
            .chain(out.abrupt)
            .map(|source| Incoming {
                node: source.node,
                edge_kind: "control:finally",
            })
            .collect();
        let finally = self.build_sequence(finally_body, entry, inner)?;
        let mut result = Flow {
            normal: Vec::new(),
            abrupt: finally.abrupt,
        };
        if entered_abruptly {
            result
                .abrupt
                .extend(finally.normal.iter().map(|tail| Incoming {
                    node: tail.node.clone(),
                    edge_kind: "control:terminate",
                }));
        }
        if entered_normally {
            result.normal = finally.normal;
        }
        Ok(result)
    }

    fn finish(mut self) -> anyhow::Result<FunctionPdg> {
        let flow = self.build_sequence(
            &self.input.body,
            vec![Incoming {
                node: self.entry.clone(),
                edge_kind: "control:entry",
            }],
            0,
        )?;
        for tail in flow.normal.into_iter().chain(flow.abrupt) {
            self.add_control(&tail.node, &self.exit.clone(), tail.edge_kind);
        }
        self.add_data_edges()?;
        self.edges.sort_by(|left, right| {
            (
                &left.source_node,
                &left.target_node,
                &left.edge_kind,
                &left.variable,
            )
                .cmp(&(
                    &right.source_node,
                    &right.target_node,
                    &right.edge_kind,
                    &right.variable,
                ))
        });
        self.edges.dedup();
        Ok(FunctionPdg {
            function_name: self.input.function_name.clone(),
            generation_id: self.input.generation_id,
            content_hash: self.input.content_hash,
            nodes: self.nodes,
            edges: self.edges,
        })
    }

    /// Pass ceiling shared by both dataflow fixpoints below.
    ///
    /// Sound for each, not a guess. Reaching definitions: a definition reaches
    /// a node along some shortest path, a shortest path is simple and so has at
    /// most `|N|` edges, and every pass sweeps every node — so after `|N|`
    /// passes every definition has arrived and pass `|N| + 1` reports no change.
    /// Taint: a node's whole definition set is tainted in the single pass its
    /// `consumes_taint` first holds, and a node can make that transition once,
    /// so at most `|N|` passes are productive.
    ///
    /// Hitting it therefore means the iteration is not the monotone one this
    /// argument describes, which is a bug in this module rather than a large
    /// input — so it is reported, not absorbed.
    fn fixpoint_pass_ceiling(&self) -> usize {
        self.nodes.len().saturating_add(2)
    }

    fn add_data_edges(&mut self) -> anyhow::Result<()> {
        type Definitions = BTreeMap<String, BTreeSet<(String, u32)>>;
        let mut predecessors: BTreeMap<String, BTreeSet<String>> = self
            .nodes
            .keys()
            .map(|node| (node.clone(), BTreeSet::new()))
            .collect();
        for edge in self
            .edges
            .iter()
            .filter(|edge| edge.edge_kind.starts_with("control:"))
        {
            predecessors
                .entry(edge.target_node.clone())
                .or_default()
                .insert(edge.source_node.clone());
        }
        let mut inbound: BTreeMap<String, Definitions> = self
            .nodes
            .keys()
            .map(|node| (node.clone(), Definitions::new()))
            .collect();
        let mut outbound = inbound.clone();
        let ceiling = self.fixpoint_pass_ceiling();
        // Every lookup below goes through `get`, never `BTreeMap`'s `Index`.
        // Both maps are keyed by every node id and every id used here comes
        // from `self.nodes`, so a miss is impossible today — and `Index` turns
        // the day that stops being true into a panic in a library, which is the
        // one failure mode a caller cannot do anything about.
        let empty = Definitions::new();
        let mut passes = 0usize;
        loop {
            passes += 1;
            if passes > ceiling {
                anyhow::bail!(
                    "PDG reaching definitions did not settle in {ceiling} pass(es) over \
                     {} node(s)",
                    self.nodes.len()
                );
            }
            let mut changed = false;
            for (node, cfg_node) in &self.nodes {
                let mut new_in = Definitions::new();
                for predecessor in predecessors.get(node).into_iter().flatten() {
                    merge_definitions(&mut new_in, outbound.get(predecessor).unwrap_or(&empty));
                }
                let mut new_out = new_in.clone();
                if let Some(facts) = self.facts.get(node) {
                    for definition in &facts.definitions {
                        new_out.insert(
                            definition.clone(),
                            [(node.clone(), cfg_node.leader_line)].into_iter().collect(),
                        );
                    }
                }
                if inbound.get(node) != Some(&new_in) || outbound.get(node) != Some(&new_out) {
                    inbound.insert(node.clone(), new_in);
                    outbound.insert(node.clone(), new_out);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        let mut tainted: BTreeSet<(String, String)> = self
            .input
            .params
            .iter()
            .map(|param| (self.entry.clone(), param.clone()))
            .collect();
        let mut passes = 0usize;
        loop {
            passes += 1;
            if passes > ceiling {
                anyhow::bail!(
                    "PDG taint propagation did not settle in {ceiling} pass(es) over {} node(s)",
                    self.nodes.len()
                );
            }
            let mut changed = false;
            for (node, facts) in &self.facts {
                let reaching = inbound.get(node).unwrap_or(&empty);
                let consumes_taint = facts.uses.iter().any(|used| {
                    reaching.get(used).is_some_and(|definitions| {
                        definitions
                            .iter()
                            .any(|(source, _)| tainted.contains(&(source.clone(), used.clone())))
                    })
                });
                if consumes_taint {
                    for definition in &facts.definitions {
                        changed |= tainted.insert((node.clone(), definition.clone()));
                    }
                }
            }
            if !changed {
                break;
            }
        }

        let mut data_edges = Vec::new();
        for (node, facts) in &self.facts {
            let reaching = inbound.get(node).unwrap_or(&empty);
            for used in &facts.uses {
                for (source, _) in reaching.get(used).into_iter().flatten() {
                    data_edges.push(PdgEdge {
                        source_node: source.clone(),
                        target_node: node.clone(),
                        edge_kind: if source == &self.entry {
                            "param".to_string()
                        } else {
                            "data".to_string()
                        },
                        variable: Some(used.clone()),
                    });
                }
            }
            for sink in &facts.sinks {
                for (source, _) in reaching.get(sink).into_iter().flatten() {
                    if tainted.contains(&(source.clone(), sink.clone())) {
                        data_edges.push(PdgEdge {
                            source_node: source.clone(),
                            target_node: node.clone(),
                            edge_kind: "taint".to_string(),
                            variable: Some(sink.clone()),
                        });
                    }
                }
            }
        }
        self.edges.extend(data_edges);
        Ok(())
    }
}

fn node_facts(definitions: &[String], uses: &[String], sinks: &[String]) -> NodeFacts {
    NodeFacts {
        definitions: definitions.iter().cloned().collect(),
        uses: uses.iter().cloned().collect(),
        sinks: sinks.iter().cloned().collect(),
    }
}

fn merge_definitions(
    destination: &mut BTreeMap<String, BTreeSet<(String, u32)>>,
    source: &BTreeMap<String, BTreeSet<(String, u32)>>,
) {
    for (variable, definitions) in source {
        destination
            .entry(variable.clone())
            .or_default()
            .extend(definitions.iter().cloned());
    }
}

pub fn build_function_pdg(input: &FunctionPdgInput) -> anyhow::Result<FunctionPdg> {
    if input.function_name.trim().is_empty() {
        anyhow::bail!("PDG function name must not be empty");
    }
    if input.start_line == 0 || input.end_line < input.start_line {
        anyhow::bail!("PDG function line range is invalid");
    }
    let mut variables = BTreeSet::new();
    for param in &input.params {
        if param.trim().is_empty() || !variables.insert(param) {
            anyhow::bail!("PDG parameter names must be unique and non-empty");
        }
    }
    let mut budget = Budget {
        statements: MAX_PDG_STATEMENTS,
    };
    validate_statements(
        &input.body,
        input.start_line,
        input.end_line,
        0,
        &mut budget,
    )?;
    PdgBuilder::new(input).finish()
}

/// What is left of the size ceilings while one input is being validated.
struct Budget {
    statements: usize,
}

/// Check the statement tree, and bound the walk that checks it.
///
/// `depth` and `budget` are the two ceilings the module documents. They are
/// enforced here, before a node is allocated, because this is the walk that
/// runs first: refusing an oversized tree at the gate is what keeps
/// `build_sequence` from descending it and what keeps the fixpoints from
/// sweeping it.
fn validate_statements(
    statements: &[PdgStatement],
    start_line: u32,
    end_line: u32,
    depth: usize,
    budget: &mut Budget,
) -> anyhow::Result<()> {
    if depth > MAX_PDG_NESTING_DEPTH {
        anyhow::bail!("PDG statement nesting exceeds the {MAX_PDG_NESTING_DEPTH}-level depth cap");
    }
    let inner = depth + 1;
    for statement in statements {
        validate_point(
            statement.line,
            &statement.definitions,
            &statement.uses,
            &statement.taint_sinks,
            start_line,
            end_line,
            budget,
        )?;
        match &statement.kind {
            PdgStatementKind::Branch {
                then_body,
                elif_arms,
                else_body,
            } => {
                validate_statements(then_body, start_line, end_line, inner, budget)?;
                // Iterated, not recursed: each arm costs one statement of the
                // budget and its body is one level in, like `then_body`.
                for arm in elif_arms {
                    validate_point(
                        arm.line,
                        &arm.definitions,
                        &arm.uses,
                        &arm.taint_sinks,
                        start_line,
                        end_line,
                        budget,
                    )?;
                    validate_statements(&arm.body, start_line, end_line, inner, budget)?;
                }
                validate_statements(else_body, start_line, end_line, inner, budget)?;
            }
            PdgStatementKind::Loop { body } => {
                validate_statements(body, start_line, end_line, inner, budget)?;
            }
            PdgStatementKind::Try {
                body,
                handlers,
                finally_body,
            } => {
                validate_statements(body, start_line, end_line, inner, budget)?;
                for handler in handlers {
                    validate_statements(handler, start_line, end_line, inner, budget)?;
                }
                validate_statements(finally_body, start_line, end_line, inner, budget)?;
            }
            PdgStatementKind::Basic | PdgStatementKind::Return | PdgStatementKind::Raise => {}
        }
    }
    Ok(())
}

/// The checks every CFG point carries — a statement or an `elif` test: one
/// statement of the budget, a line inside the function, non-empty variables,
/// and every sink an exact use.
fn validate_point(
    line: u32,
    definitions: &[String],
    uses: &[String],
    taint_sinks: &[String],
    start_line: u32,
    end_line: u32,
    budget: &mut Budget,
) -> anyhow::Result<()> {
    let Some(remaining) = budget.statements.checked_sub(1) else {
        anyhow::bail!(
            "PDG function exceeds the {MAX_PDG_STATEMENTS}-statement cap; \
             the graph is refused rather than built from part of the function"
        );
    };
    budget.statements = remaining;
    if !(start_line..=end_line).contains(&line) {
        anyhow::bail!("PDG statement line {line} is outside the function");
    }
    if definitions
        .iter()
        .chain(uses)
        .chain(taint_sinks)
        .any(|variable| variable.trim().is_empty())
    {
        anyhow::bail!("PDG variables must not be empty");
    }
    let uses: BTreeSet<_> = uses.iter().collect();
    if taint_sinks.iter().any(|sink| !uses.contains(sink)) {
        anyhow::bail!("every PDG taint sink must be an exact statement use");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statement(
        line: u32,
        definitions: &[&str],
        uses: &[&str],
        kind: PdgStatementKind,
    ) -> PdgStatement {
        PdgStatement {
            line,
            definitions: definitions
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            uses: uses.iter().map(|value| (*value).to_string()).collect(),
            taint_sinks: Vec::new(),
            kind,
        }
    }

    fn node_at(pdg: &FunctionPdg, line: u32) -> &str {
        pdg.nodes
            .values()
            .find(|node| {
                node.leader_line == line
                    && !matches!(node.kind, CfgNodeKind::Entry | CfgNodeKind::Exit)
            })
            .map(|node| node.id.as_str())
            .unwrap_or_else(|| panic!("missing node at line {line}"))
    }

    #[test]
    fn cfg_threads_branch_loop_try_and_terminators() {
        let input = FunctionPdgInput {
            function_name: "process".into(),
            generation_id: 7,
            content_hash: 99,
            start_line: 10,
            end_line: 30,
            params: vec!["arg".into()],
            body: vec![
                statement(
                    11,
                    &[],
                    &["arg"],
                    PdgStatementKind::Branch {
                        then_body: vec![statement(
                            12,
                            &["value"],
                            &["arg"],
                            PdgStatementKind::Basic,
                        )],
                        elif_arms: Vec::new(),
                        else_body: vec![statement(13, &["value"], &[], PdgStatementKind::Basic)],
                    },
                ),
                statement(
                    14,
                    &[],
                    &["value"],
                    PdgStatementKind::Loop {
                        body: vec![statement(
                            15,
                            &["value"],
                            &["value"],
                            PdgStatementKind::Basic,
                        )],
                    },
                ),
                statement(
                    16,
                    &[],
                    &[],
                    PdgStatementKind::Try {
                        body: vec![statement(17, &[], &["value"], PdgStatementKind::Basic)],
                        handlers: vec![vec![statement(18, &[], &["arg"], PdgStatementKind::Raise)]],
                        finally_body: vec![statement(19, &[], &["value"], PdgStatementKind::Basic)],
                    },
                ),
                statement(20, &[], &["value"], PdgStatementKind::Return),
            ],
        };
        let pdg = build_function_pdg(&input).unwrap();
        assert_eq!(pdg.generation_id, 7);
        assert_eq!(pdg.content_hash, 99);
        for line in 11..=20 {
            if line != 21 {
                assert!(pdg.nodes.values().any(|node| node.leader_line == line));
            }
        }
        let loop_header = node_at(&pdg, 14);
        let loop_body = node_at(&pdg, 15);
        assert!(pdg.edges.iter().any(|edge| {
            edge.source_node == loop_body
                && edge.target_node == loop_header
                && edge.edge_kind == "control:loop"
        }));
        let try_header = node_at(&pdg, 16);
        for (line, kind) in [(17, "control:try"), (18, "control:exception")] {
            let target = node_at(&pdg, line);
            assert!(pdg.edges.iter().any(|edge| {
                edge.source_node == try_header
                    && edge.target_node == target
                    && edge.edge_kind == kind
            }));
        }
        // The raise is inside a `try` with a `finally`, so it leaves through
        // the finally (line 19) and the finally's tail carries it to exit.
        // This used to assert a raise → exit edge that bypassed the finally,
        // which is the defect devmap-query-01 names: a finally never on a
        // terminating path.
        let raise = node_at(&pdg, 18);
        let finally = node_at(&pdg, 19);
        assert!(pdg.edges.iter().any(|edge| {
            edge.source_node == raise
                && edge.target_node == finally
                && edge.edge_kind == "control:finally"
        }));
        assert!(pdg.edges.iter().any(|edge| {
            edge.source_node == finally
                && edge.target_node == "process:exit"
                && edge.edge_kind == "control:terminate"
        }));
        assert!(!pdg
            .edges
            .iter()
            .any(|edge| { edge.source_node == raise && edge.target_node == "process:exit" }));
    }

    #[test]
    fn reaching_definitions_keep_loop_carried_edges_and_exact_taint() {
        let mut sink = statement(14, &[], &["value", "value_extra"], PdgStatementKind::Return);
        sink.taint_sinks = vec!["value".into()];
        let input = FunctionPdgInput {
            function_name: "flow".into(),
            generation_id: 1,
            content_hash: 2,
            start_line: 10,
            end_line: 20,
            params: vec!["source".into(), "value_extra".into()],
            body: vec![
                statement(11, &["value"], &["source"], PdgStatementKind::Basic),
                statement(
                    12,
                    &[],
                    &["value"],
                    PdgStatementKind::Loop {
                        body: vec![statement(
                            13,
                            &["value"],
                            &["value"],
                            PdgStatementKind::Basic,
                        )],
                    },
                ),
                sink,
            ],
        };
        let pdg = build_function_pdg(&input).unwrap();
        let loop_header = node_at(&pdg, 12);
        let loop_body = node_at(&pdg, 13);
        assert!(pdg.edges.iter().any(|edge| {
            edge.source_node == loop_body
                && edge.target_node == loop_header
                && edge.edge_kind == "data"
                && edge.variable.as_deref() == Some("value")
        }));
        let sink_node = node_at(&pdg, 14);
        assert!(pdg.edges.iter().any(|edge| {
            edge.target_node == sink_node
                && edge.edge_kind == "taint"
                && edge.variable.as_deref() == Some("value")
        }));
        assert!(!pdg.edges.iter().any(|edge| {
            edge.target_node == sink_node
                && edge.edge_kind == "taint"
                && edge.variable.as_deref() == Some("value_extra")
        }));
    }

    fn has_edge(pdg: &FunctionPdg, source: &str, target: &str, kind: &str) -> bool {
        pdg.edges.iter().any(|edge| {
            edge.source_node == source && edge.target_node == target && edge.edge_kind == kind
        })
    }

    fn sink(line: u32, variable: &str) -> PdgStatement {
        let mut statement = statement(line, &[], &[variable], PdgStatementKind::Basic);
        statement.taint_sinks = vec![variable.to_string()];
        statement
    }

    fn function(params: &[&str], body: Vec<PdgStatement>) -> FunctionPdgInput {
        FunctionPdgInput {
            function_name: "f".into(),
            generation_id: 1,
            content_hash: 2,
            start_line: 1,
            end_line: 40,
            params: params.iter().map(|value| (*value).to_string()).collect(),
            body,
        }
    }

    /// devmap-query-01: `try: return` / `finally: sink(arg)`. The finally runs
    /// on the way out of the return, so it must get a node, a taint edge, a
    /// control edge from the return, and a path to exit.
    #[test]
    fn finally_runs_on_the_path_out_of_a_return_or_raise() {
        let input = function(
            &["arg"],
            vec![statement(
                2,
                &[],
                &[],
                PdgStatementKind::Try {
                    body: vec![statement(3, &[], &["arg"], PdgStatementKind::Return)],
                    handlers: vec![vec![statement(5, &[], &["arg"], PdgStatementKind::Raise)]],
                    finally_body: vec![sink(7, "arg")],
                },
            )],
        );
        let pdg = build_function_pdg(&input).unwrap();
        let finally = node_at(&pdg, 7).to_string();
        let ret = node_at(&pdg, 3).to_string();
        let raise = node_at(&pdg, 5).to_string();
        assert!(
            has_edge(&pdg, &ret, &finally, "control:finally"),
            "{:#?}",
            pdg.edges
        );
        assert!(has_edge(&pdg, &raise, &finally, "control:finally"));
        assert!(
            has_edge(&pdg, &finally, "f:exit", "control:terminate"),
            "the finally tail must reach exit: {:#?}",
            pdg.edges
        );
        // Neither terminator bypasses the finally on its way out.
        assert!(!has_edge(&pdg, &ret, "f:exit", "control:terminate"));
        assert!(!has_edge(&pdg, &raise, "f:exit", "control:terminate"));
        assert!(pdg.edges.iter().any(|edge| {
            edge.target_node == finally
                && edge.edge_kind == "taint"
                && edge.variable.as_deref() == Some("arg")
        }));
    }

    #[test]
    fn statements_after_a_return_in_a_plain_sequence_are_not_emitted() {
        let input = function(
            &["arg"],
            vec![
                statement(2, &[], &["arg"], PdgStatementKind::Return),
                sink(3, "arg"),
            ],
        );
        let pdg = build_function_pdg(&input).unwrap();
        assert!(!pdg.nodes.values().any(|node| node.leader_line == 3));
        let ret = node_at(&pdg, 2);
        assert!(has_edge(&pdg, ret, "f:exit", "control:terminate"));
    }

    /// devmap-query-02: an exception can leave the try body after any of its
    /// statements, so a definition made there reaches the handler.
    #[test]
    fn a_try_body_definition_reaches_a_handler_use() {
        let input = function(
            &["arg"],
            vec![statement(
                2,
                &[],
                &[],
                PdgStatementKind::Try {
                    body: vec![
                        statement(3, &["value"], &["arg"], PdgStatementKind::Basic),
                        statement(4, &[], &["value"], PdgStatementKind::Basic),
                    ],
                    handlers: vec![vec![sink(6, "value")]],
                    finally_body: Vec::new(),
                },
            )],
        );
        let pdg = build_function_pdg(&input).unwrap();
        let definition = node_at(&pdg, 3);
        let handler = node_at(&pdg, 6);
        for kind in ["data", "taint"] {
            assert!(
                pdg.edges.iter().any(|edge| {
                    edge.source_node == definition
                        && edge.target_node == handler
                        && edge.edge_kind == kind
                        && edge.variable.as_deref() == Some("value")
                }),
                "missing {kind} edge into the handler: {:#?}",
                pdg.edges
            );
        }
        // The try header still enters the handler directly: an exception can
        // be raised before the first body statement completes.
        let header = node_at(&pdg, 2);
        assert!(has_edge(&pdg, header, handler, "control:exception"));
    }

    #[test]
    fn a_65_deep_nested_if_is_still_refused() {
        let mut body = vec![statement(2, &[], &[], PdgStatementKind::Basic)];
        for _ in 0..=MAX_PDG_NESTING_DEPTH {
            body = vec![statement(
                2,
                &[],
                &[],
                PdgStatementKind::Branch {
                    then_body: body,
                    elif_arms: Vec::new(),
                    else_body: Vec::new(),
                },
            )];
        }
        let error = build_function_pdg(&function(&[], body)).unwrap_err();
        assert!(error.to_string().contains("depth cap"), "{error}");
    }

    #[test]
    fn invalid_ranges_and_non_exact_sinks_fail_closed() {
        let mut bad = statement(12, &[], &["value"], PdgStatementKind::Basic);
        bad.taint_sinks = vec!["value_suffix".into()];
        let input = FunctionPdgInput {
            function_name: "bad".into(),
            generation_id: 1,
            content_hash: 2,
            start_line: 10,
            end_line: 11,
            params: Vec::new(),
            body: vec![bad],
        };
        assert!(build_function_pdg(&input).is_err());
    }
}
