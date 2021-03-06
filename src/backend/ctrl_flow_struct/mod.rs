//! Recovers high-level control-flow constructs from a control-flow graph.
//! Implements the algorithm described in
//! [*No More Gotos*](https://doi.org/10.14722/ndss.2015.23185)

#![allow(dead_code)]

pub mod ast_context;
mod condition;
mod graph_utils;
#[cfg(test)]
mod tests;

use self::ast_context::*;
use self::condition::*;
use self::graph_utils::ix_bit_set::IxBitSet;

use petgraph::stable_graph::{EdgeIndex, NodeIndex, StableDiGraph};
use petgraph::visit::EdgeRef;
use petgraph::Incoming;

use std::collections::HashMap;
use std::fmt;
use std::iter::{self, FromIterator};
use std::mem;

struct ControlFlowGraph<'cd, A: AstContext> {
    graph: StableDiGraph<CfgNode<'cd, A>, Option<Condition<'cd, A>>>,
    entry: NodeIndex,
    cctx: ConditionContext<'cd, A::Condition>,
    actx: A,
}

type NodeSet = IxBitSet<NodeIndex>;
type EdgeSet = IxBitSet<EdgeIndex>;

enum CfgNode<'cd, A: AstContext> {
    /// out-degree <= 1; out-edge weights are `None`
    Code(AstNode<'cd, A>),
    /// out-degree >= 2; out-edge weights are `Some(..)`
    Condition,
    /// only appears temporarily in the middle of algorithms
    Dummy(&'static str),
}

enum AstNode<'cd, A: AstContext> {
    BasicBlock(A::Block),
    Seq(Vec<AstNode<'cd, A>>),
    Cond(
        Condition<'cd, A>,
        Box<AstNode<'cd, A>>,
        Option<Box<AstNode<'cd, A>>>,
    ),
    Loop(LoopType<'cd, A>, Box<AstNode<'cd, A>>),
    Switch(
        A::Variable,
        Vec<(ValueSet, AstNode<'cd, A>)>,
        Box<AstNode<'cd, A>>,
    ),
}

enum LoopType<'cd, A: AstContext> {
    PreChecked(Condition<'cd, A>),
    PostChecked(Condition<'cd, A>),
    Endless,
}

type ValueSet = (); // XXX

type Condition<'cd, A> = condition::BaseCondition<'cd, <A as AstContext>::Condition>;

impl<'cd, A: AstContext> ControlFlowGraph<'cd, A> {
    fn structure_whole(mut self) -> AstNode<'cd, A> {
        let mut backedge_map = HashMap::new();
        let mut podfs_trace = Vec::new();
        graph_utils::depth_first_search(&self.graph, self.entry, |ev| {
            use self::graph_utils::DfsEvent::*;
            match ev {
                BackEdge(e) => {
                    let (ref mut sources, ref mut edges) = backedge_map
                        .entry(e.target())
                        .or_insert((NodeSet::new(), EdgeSet::new()));
                    sources.insert(e.source());
                    edges.insert(e.id());
                }
                Finish(n) => podfs_trace.push(n),
                _ => (),
            }
        });

        for &n in &podfs_trace {
            if let Some((latch_nodes, backedges)) = backedge_map.get(&n) {
                // loop

                // retarget backedges to a "loop continue" node
                let loop_continue = self.graph.add_node(CfgNode::Dummy("loop continue"));
                for backedge in backedges {
                    graph_utils::retarget_edge(&mut self.graph, backedge, loop_continue);
                }

                let (initial_loop_nodes, _, _) = graph_utils::slice(&self.graph, n, latch_nodes);

                let loop_header = self.funnel_abnormal_entries(n, &initial_loop_nodes);

                let loop_succ_opt = {
                    let mut succ_nodes =
                        graph_utils::strict_successors_of_set(&self.graph, &initial_loop_nodes);
                    let mut loop_nodes = initial_loop_nodes;
                    self.refine_loop(&mut loop_nodes, &mut succ_nodes);

                    // pick the successor with the smallest post-order
                    let final_succ_opt = podfs_trace
                        .iter()
                        .find(|&&n| succ_nodes.contains(n))
                        .cloned();
                    if let Some(final_succ) = final_succ_opt {
                        succ_nodes.remove(final_succ);
                        Some(self.funnel_abnormal_exits(
                            &loop_nodes,
                            loop_continue,
                            final_succ,
                            &succ_nodes,
                        ))
                    } else {
                        None
                    }
                };

                let loop_body = self.structure_acyclic_sese_region(loop_header, loop_continue);
                self.graph.remove_node(loop_continue);
                let repl_ast = AstNode::Loop(LoopType::Endless, Box::new(loop_body));
                self.graph[loop_header] = CfgNode::Code(repl_ast);
                if let Some(loop_succ) = loop_succ_opt {
                    self.graph.add_edge(loop_header, loop_succ, None);
                }
            } else {
                // acyclic
                let region = graph_utils::dominated_by(&self.graph, self.entry, n);
                // single-block regions aren't interesting
                if region.len() > 1 {
                    let succs = graph_utils::strict_successors_of_set(&self.graph, &region);
                    // is `region` single-exit?
                    let mut succs_iter = succs.iter();
                    if let Some(succ) = succs_iter.next() {
                        if succs_iter.next().is_none() {
                            // sese region
                            let repl_ast = self.structure_acyclic_sese_region(n, succ);
                            self.graph[n] = CfgNode::Code(repl_ast);
                            self.graph.add_edge(n, succ, None);
                        }
                    }
                }
            }
        }

        // connect all remaining sinks to a dummy node
        let dummy_exit = self.graph.add_node(CfgNode::Dummy("exit"));
        let sinks: Vec<_> = self
            .graph
            .node_indices()
            .filter(|&n| self.graph.edges(n).next().is_none())
            .collect();
        for n in sinks {
            self.graph.add_edge(n, dummy_exit, None);
        }

        let entry = self.entry;
        let ret = self.structure_acyclic_sese_region(entry, dummy_exit);

        self.graph.remove_node(dummy_exit);
        self.graph.remove_node(self.entry);
        debug_assert!(self.graph.node_count() == 0);

        ret
    }

    /// Converts the acyclic, single entry, single exit region bound by `header`
    /// and `successor` into an `AstNode`.
    fn structure_acyclic_sese_region(
        &mut self,
        header: NodeIndex,
        successor: NodeIndex,
    ) -> AstNode<'cd, A> {
        let (reaching_conds, mut region_topo_order) =
            self.reaching_conditions(header, &NodeSet::from_iter(&[successor]));
        // slice includes `successor`, but its not actually part of the region.
        let _popped = region_topo_order.pop();
        debug_assert!(_popped == Some(successor));

        // remove all region nodes from the cfg and add them to an AstNode::Seq
        AstNode::Seq(
            region_topo_order
                .iter()
                .filter_map(|&n| {
                    let cfg_node = if n == header {
                        // we don't want to remove `header` since that will also remove
                        // incoming edges, which we need to keep
                        // instead we replace it with a dummy value that will be
                        // later replaced with the actual value
                        mem::replace(&mut self.graph[header], CfgNode::Dummy("replaced header"))
                    } else {
                        self.graph.remove_node(n).unwrap()
                    };
                    if let CfgNode::Code(ast) = cfg_node {
                        Some(AstNode::Cond(reaching_conds[&n], Box::new(ast), None))
                    } else {
                        None
                    }
                })
                .collect(),
        )
    }

    /// Computes the reaching condition for every node in the graph slice from
    /// `start` to `end_set`. Also returns a topological ordering of that slice.
    fn reaching_conditions(
        &self,
        start: NodeIndex,
        end_set: &NodeSet,
    ) -> (HashMap<NodeIndex, Condition<'cd, A>>, Vec<NodeIndex>) {
        let (_, slice_edges, slice_topo_order) = graph_utils::slice(&self.graph, start, end_set);
        // {Node, Edge}Filtered don't implement IntoNeighborsDirected :(
        // https://github.com/bluss/petgraph/pull/219
        // Also EdgeFiltered<Reversed<_>, _> isn't Into{Neighbors, Edges}
        // because Reversed<_> isn't IntoEdges

        let mut ret = HashMap::with_capacity(slice_topo_order.len());

        {
            let mut iter = slice_topo_order.iter();
            let _first = iter.next();
            debug_assert!(_first == Some(&start));
            ret.insert(start, self.cctx.mk_true());

            for &n in iter {
                let reach_cond = self.cctx.mk_or_from_iter(
                    // manually restrict to slice
                    self.graph
                        .edges_directed(n, Incoming)
                        .filter(|e| slice_edges.contains(e.id()))
                        .map(|e| {
                            if let &Some(ec) = e.weight() {
                                self.cctx.mk_and(ret[&e.source()], ec)
                            } else {
                                ret[&e.source()]
                            }
                        }),
                );
                let _old = ret.insert(n, reach_cond);
                debug_assert!(_old.is_none());
            }
        }

        (ret, slice_topo_order)
    }

    /// Transforms the loop into a single-entry loop.
    /// Returns the new loop header.
    fn funnel_abnormal_entries(&mut self, header: NodeIndex, loop_nodes: &NodeSet) -> NodeIndex {
        if header == self.entry {
            // all entries must go through `entry` aka `header`
            return header;
        }

        let mut entry_map = HashMap::new();
        for n in loop_nodes {
            for e in self.graph.edges_directed(n, Incoming) {
                if !loop_nodes.contains(e.source()) {
                    entry_map.entry(n).or_insert(Vec::new()).push(e.id());
                }
            }
        }
        // loop must be reachable, so the header must have entries
        let header_entries = entry_map.remove(&header).unwrap();
        debug_assert!(!header_entries.is_empty());
        let abnormal_entry_map = entry_map;
        if abnormal_entry_map.is_empty() {
            // no abnormal entries
            return header;
        }
        let abnormal_entry_iter = (1..).zip(&abnormal_entry_map);

        let struct_var = self.actx.mk_fresh_var();

        // make condition cascade
        let new_header = {
            let abnormal_entry_iter = abnormal_entry_iter.clone().map(|(n, (&t, _))| (n, t));

            let dummy_preheader = self.graph.add_node(CfgNode::Dummy("loop \"preheader\""));

            let mut prev_cascade_node = dummy_preheader;
            let mut prev_entry_target = header;
            let mut prev_out_cond = None;
            let mut prev_entry_num = 0;

            // we make the condition node for the *previous* entry target b/c
            // the current one might be the last one, which shouldn't get a
            // condition node because it's the only possible target
            for (entry_num, entry_target) in abnormal_entry_iter {
                let prev_cond_eq = self
                    .cctx
                    .mk_simple(self.actx.mk_cond_equals(&struct_var, prev_entry_num));
                let cascade_node = self.graph.add_node(CfgNode::Condition);
                self.graph
                    .add_edge(prev_cascade_node, cascade_node, prev_out_cond);
                self.graph
                    .add_edge(cascade_node, prev_entry_target, Some(prev_cond_eq));

                let struct_reset = self.graph.add_node(CfgNode::Code(AstNode::BasicBlock(
                    self.actx.mk_var_assign(&struct_var, 0),
                )));
                self.graph.add_edge(struct_reset, entry_target, None);

                prev_cascade_node = cascade_node;
                prev_entry_target = struct_reset;
                prev_out_cond = Some(self.cctx.mk_not(prev_cond_eq));
                prev_entry_num = entry_num;
            }

            self.graph
                .add_edge(prev_cascade_node, prev_entry_target, prev_out_cond);

            // we always add an edge from dummy_preheader
            let new_header = self.graph.neighbors(dummy_preheader).next().unwrap();
            self.graph.remove_node(dummy_preheader);
            new_header
        };

        // redirect entries
        for (entry_num, entry_edges) in
            iter::once((0, &header_entries)).chain(abnormal_entry_iter.map(|(n, (_, e))| (n, e)))
        {
            let struct_assign = self.graph.add_node(CfgNode::Code(AstNode::BasicBlock(
                self.actx.mk_var_assign(&struct_var, entry_num),
            )));
            self.graph.add_edge(struct_assign, new_header, None);
            for &entry_edge in entry_edges {
                graph_utils::retarget_edge(&mut self.graph, entry_edge, struct_assign);
            }
        }

        new_header
    }

    /// Incrementally adds nodes dominated by the loop to the loop until
    /// there's only one successor or there are no more nodes to add.
    fn refine_loop(&self, loop_nodes: &mut NodeSet, succ_nodes: &mut NodeSet) -> () {
        // reuse this `NodeSet` so we avoid allocating
        let mut new_nodes = NodeSet::new();
        while succ_nodes.len() > 1 {
            for n in &*succ_nodes {
                if self
                    .graph
                    .neighbors_directed(n, Incoming)
                    .all(|pred| loop_nodes.contains(pred))
                {
                    // post-pone removal from `succ_nodes` b/c rust ownership
                    loop_nodes.insert(n);
                    new_nodes.extend(self.graph.neighbors(n).filter(|&u| !loop_nodes.contains(u)));
                }
            }

            // do the removal
            succ_nodes.difference_with(&loop_nodes);

            if new_nodes.is_empty() {
                break;
            }
            succ_nodes.union_with(&new_nodes);
            new_nodes.clear();
        }
    }

    /// Transforms the loop so that all loop exits are `break`.
    /// Returns the new loop successor.
    fn funnel_abnormal_exits(
        &mut self,
        loop_nodes: &NodeSet,
        loop_continue: NodeIndex,
        final_succ: NodeIndex,
        abn_succ_nodes: &NodeSet,
    ) -> NodeIndex {
        let new_successor = if !abn_succ_nodes.is_empty() {
            let reaching_conds = {
                let abn_exit_sources: NodeSet = abn_succ_nodes
                    .iter()
                    .flat_map(|n| self.graph.edges_directed(n, Incoming))
                    .map(|e| e.source())
                    .filter(|&n| loop_nodes.contains(n))
                    .collect();

                let ncd = graph_utils::nearest_common_dominator(
                    &self.graph,
                    self.entry,
                    &abn_exit_sources,
                );
                self.reaching_conditions(ncd, abn_succ_nodes).0
            };

            // make condition cascade
            {
                let dummy_presuccessor =
                    self.graph.add_node(CfgNode::Dummy("loop \"presuccessor\""));

                let mut prev_cascade_node = dummy_presuccessor;
                let mut prev_out_cond = None;

                for exit_target in abn_succ_nodes {
                    let reaching_cond = reaching_conds[&exit_target];
                    let cascade_node = self.graph.add_node(CfgNode::Condition);
                    self.graph
                        .add_edge(prev_cascade_node, cascade_node, prev_out_cond);
                    self.graph
                        .add_edge(cascade_node, exit_target, Some(reaching_cond));
                    prev_cascade_node = cascade_node;
                    prev_out_cond = Some(self.cctx.mk_not(reaching_cond));
                }

                self.graph
                    .add_edge(prev_cascade_node, final_succ, prev_out_cond);

                // we always add an edge from dummy_presuccessor
                let new_successor = self.graph.neighbors(dummy_presuccessor).next().unwrap();
                self.graph.remove_node(dummy_presuccessor);
                new_successor
            }
        } else {
            final_succ
        };

        // replace exit edges with "break"
        let exit_edges: EdgeSet = loop_nodes
            .iter()
            .flat_map(|n| self.graph.edges(n))
            .filter(|e| !loop_nodes.contains(e.target()))
            .map(|e| e.id())
            .collect();
        for exit_edge in &exit_edges {
            let break_node = self
                .graph
                .add_node(CfgNode::Code(AstNode::BasicBlock(self.actx.mk_break())));
            graph_utils::retarget_edge(&mut self.graph, exit_edge, break_node);
            // connect to `loop_continue` so that the graph slice from the loop
            // header to `loop_continue` contains these "break" nodes
            self.graph
                .add_edge(break_node, loop_continue, Some(self.cctx.mk_false()));
        }

        new_successor
    }
}

impl<'cd, A> fmt::Debug for ControlFlowGraph<'cd, A>
where
    A: AstContext + fmt::Debug,
    A::Block: fmt::Debug,
    A::Variable: fmt::Debug,
    A::Condition: fmt::Debug,
{
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("ControlFlowGraph")
            .field("graph", &self.graph)
            .field("entry", &self.entry)
            .field("cctx", &self.cctx)
            .field("actx", &self.actx)
            .finish()
    }
}

impl<'cd, A> fmt::Debug for CfgNode<'cd, A>
where
    A: AstContext,
    A::Block: fmt::Debug,
    A::Variable: fmt::Debug,
    A::Condition: fmt::Debug,
{
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            CfgNode::Code(c) => fmt.debug_tuple("Code").field(c).finish(),
            CfgNode::Condition => fmt.write_str("Condition"),
            CfgNode::Dummy(s) => fmt.debug_tuple("Dummy").field(s).finish(),
        }
    }
}

impl<'cd, A> fmt::Debug for AstNode<'cd, A>
where
    A: AstContext,
    A::Block: fmt::Debug,
    A::Variable: fmt::Debug,
    A::Condition: fmt::Debug,
{
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            AstNode::BasicBlock(b) => fmt.debug_tuple("BasicBlock").field(b).finish(),
            AstNode::Seq(s) => fmt.debug_tuple("Seq").field(s).finish(),
            AstNode::Cond(c, t, e) => fmt.debug_tuple("Cond").field(c).field(t).field(e).finish(),
            AstNode::Loop(t, b) => fmt.debug_tuple("Loop").field(t).field(b).finish(),
            AstNode::Switch(v, c, d) => fmt
                .debug_tuple("Switch")
                .field(v)
                .field(c)
                .field(d)
                .finish(),
        }
    }
}

impl<'cd, A> fmt::Debug for LoopType<'cd, A>
where
    A: AstContext,
    A::Condition: fmt::Debug,
{
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            LoopType::PreChecked(c) => fmt.debug_tuple("PreChecked").field(c).finish(),
            LoopType::PostChecked(c) => fmt.debug_tuple("PostChecked").field(c).finish(),
            LoopType::Endless => fmt.write_str("Endless"),
        }
    }
}
