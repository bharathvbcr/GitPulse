//! Structured intra-procedural control and data dependency graph.
//!
//! The builder accepts a grammar-owned statement tree. It does not infer
//! control flow from line numbers: branch, loop, exception, termination,
//! definitions, uses, and exact sink variables must be explicit at the
//! extraction boundary.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PdgStatementKind {
    Basic,
    Branch {
        then_body: Vec<PdgStatement>,
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

#[derive(Debug, Clone, Default)]
struct NodeFacts {
    definitions: BTreeSet<String>,
    uses: BTreeSet<String>,
    sinks: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct Incoming {
    node: String,
    edge_kind: &'static str,
}

struct PdgBuilder<'a> {
    input: &'a FunctionPdgInput,
    nodes: BTreeMap<String, CfgNode>,
    facts: BTreeMap<String, NodeFacts>,
    edges: Vec<PdgEdge>,
    next_node: usize,
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

    fn add_statement_node(&mut self, statement: &PdgStatement) -> String {
        let kind = match statement.kind {
            PdgStatementKind::Basic => CfgNodeKind::Block,
            PdgStatementKind::Branch { .. } => CfgNodeKind::Branch,
            PdgStatementKind::Loop { .. } => CfgNodeKind::Loop,
            PdgStatementKind::Try { .. } => CfgNodeKind::Try,
            PdgStatementKind::Return => CfgNodeKind::Return,
            PdgStatementKind::Raise => CfgNodeKind::Raise,
        };
        let id = format!(
            "{}:{}:n{}",
            self.input.function_name, statement.line, self.next_node
        );
        self.next_node += 1;
        self.nodes.insert(
            id.clone(),
            CfgNode {
                id: id.clone(),
                kind,
                symbol: self.input.function_name.clone(),
                leader_line: statement.line,
                line_range: (statement.line, statement.line),
            },
        );
        self.facts.insert(
            id.clone(),
            NodeFacts {
                definitions: statement.definitions.iter().cloned().collect(),
                uses: statement.uses.iter().cloned().collect(),
                sinks: statement.taint_sinks.iter().cloned().collect(),
            },
        );
        id
    }

    fn build_sequence(
        &mut self,
        statements: &[PdgStatement],
        mut incoming: Vec<Incoming>,
    ) -> Vec<Incoming> {
        for statement in statements {
            if incoming.is_empty() {
                break;
            }
            let node = self.add_statement_node(statement);
            for source in &incoming {
                self.add_control(&source.node, &node, source.edge_kind);
            }
            incoming = match &statement.kind {
                PdgStatementKind::Basic => vec![Incoming {
                    node,
                    edge_kind: "control:fallthrough",
                }],
                PdgStatementKind::Return | PdgStatementKind::Raise => {
                    self.add_control(&node, &self.exit.clone(), "control:terminate");
                    Vec::new()
                }
                PdgStatementKind::Branch {
                    then_body,
                    else_body,
                } => {
                    let then_exits = if then_body.is_empty() {
                        vec![Incoming {
                            node: node.clone(),
                            edge_kind: "control:true",
                        }]
                    } else {
                        self.build_sequence(
                            then_body,
                            vec![Incoming {
                                node: node.clone(),
                                edge_kind: "control:true",
                            }],
                        )
                    };
                    let else_exits = if else_body.is_empty() {
                        vec![Incoming {
                            node,
                            edge_kind: "control:false",
                        }]
                    } else {
                        self.build_sequence(
                            else_body,
                            vec![Incoming {
                                node,
                                edge_kind: "control:false",
                            }],
                        )
                    };
                    then_exits.into_iter().chain(else_exits).collect()
                }
                PdgStatementKind::Loop { body } => {
                    let body_exits = self.build_sequence(
                        body,
                        vec![Incoming {
                            node: node.clone(),
                            edge_kind: "control:true",
                        }],
                    );
                    for tail in body_exits {
                        self.add_control(&tail.node, &node, "control:loop");
                    }
                    vec![Incoming {
                        node,
                        edge_kind: "control:false",
                    }]
                }
                PdgStatementKind::Try {
                    body,
                    handlers,
                    finally_body,
                } => {
                    let mut exits = self.build_sequence(
                        body,
                        vec![Incoming {
                            node: node.clone(),
                            edge_kind: "control:try",
                        }],
                    );
                    for handler in handlers {
                        exits.extend(self.build_sequence(
                            handler,
                            vec![Incoming {
                                node: node.clone(),
                                edge_kind: "control:exception",
                            }],
                        ));
                    }
                    if finally_body.is_empty() {
                        exits
                    } else {
                        for incoming in &mut exits {
                            incoming.edge_kind = "control:finally";
                        }
                        self.build_sequence(finally_body, exits)
                    }
                }
            };
        }
        incoming
    }

    fn finish(mut self) -> FunctionPdg {
        let tails = self.build_sequence(
            &self.input.body,
            vec![Incoming {
                node: self.entry.clone(),
                edge_kind: "control:entry",
            }],
        );
        for tail in tails {
            self.add_control(&tail.node, &self.exit.clone(), tail.edge_kind);
        }
        self.add_data_edges();
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
        FunctionPdg {
            function_name: self.input.function_name.clone(),
            generation_id: self.input.generation_id,
            content_hash: self.input.content_hash,
            nodes: self.nodes,
            edges: self.edges,
        }
    }

    fn add_data_edges(&mut self) {
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
        loop {
            let mut changed = false;
            for (node, cfg_node) in &self.nodes {
                let mut new_in = Definitions::new();
                for predecessor in predecessors.get(node).into_iter().flatten() {
                    merge_definitions(&mut new_in, &outbound[predecessor]);
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
                if inbound[node] != new_in || outbound[node] != new_out {
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
        loop {
            let mut changed = false;
            for (node, facts) in &self.facts {
                let consumes_taint = facts.uses.iter().any(|used| {
                    inbound[node].get(used).is_some_and(|definitions| {
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
            for used in &facts.uses {
                for (source, _) in inbound[node].get(used).into_iter().flatten() {
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
                for (source, _) in inbound[node].get(sink).into_iter().flatten() {
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
    validate_statements(&input.body, input.start_line, input.end_line)?;
    Ok(PdgBuilder::new(input).finish())
}

fn validate_statements(
    statements: &[PdgStatement],
    start_line: u32,
    end_line: u32,
) -> anyhow::Result<()> {
    for statement in statements {
        if !(start_line..=end_line).contains(&statement.line) {
            anyhow::bail!(
                "PDG statement line {} is outside the function",
                statement.line
            );
        }
        let uses: BTreeSet<_> = statement.uses.iter().collect();
        if statement
            .definitions
            .iter()
            .chain(&statement.uses)
            .chain(&statement.taint_sinks)
            .any(|variable| variable.trim().is_empty())
        {
            anyhow::bail!("PDG variables must not be empty");
        }
        if statement
            .taint_sinks
            .iter()
            .any(|sink| !uses.contains(sink))
        {
            anyhow::bail!("every PDG taint sink must be an exact statement use");
        }
        match &statement.kind {
            PdgStatementKind::Branch {
                then_body,
                else_body,
            } => {
                validate_statements(then_body, start_line, end_line)?;
                validate_statements(else_body, start_line, end_line)?;
            }
            PdgStatementKind::Loop { body } => {
                validate_statements(body, start_line, end_line)?;
            }
            PdgStatementKind::Try {
                body,
                handlers,
                finally_body,
            } => {
                validate_statements(body, start_line, end_line)?;
                for handler in handlers {
                    validate_statements(handler, start_line, end_line)?;
                }
                validate_statements(finally_body, start_line, end_line)?;
            }
            PdgStatementKind::Basic | PdgStatementKind::Return | PdgStatementKind::Raise => {}
        }
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
        let raise = node_at(&pdg, 18);
        assert!(pdg.edges.iter().any(|edge| {
            edge.source_node == raise
                && edge.target_node == "process:exit"
                && edge.edge_kind == "control:terminate"
        }));
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
