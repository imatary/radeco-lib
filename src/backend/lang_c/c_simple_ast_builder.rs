//! This module is for recovering SimpleCAST from RadecoFunction.
//!
//! Usage of this module is to call `c_simple_ast_builder::recover_simple_ast(rfn)`
//! where `rfn` is an instance of `RadecoFunction`, the function returns an instance of
//! SimpleCAST and we can obtain higher level representation than Radeco IR.

use std::collections::{HashMap, HashSet};
use frontend::radeco_containers::RadecoFunction;
use middle::ir::{MOpcode, MAddress};
use middle::ssa::utils;
use middle::ssa::ssastorage::{NodeData, SSAStorage};
use middle::ssa::ssa_traits::{SSA, SSAExtra, SSAMod, SSAWalk, ValueInfo};
use middle::ssa::cfg_traits::CFG;
use super::c_simple_ast::{ValueNode, SimpleCAST, SimpleCASTEdge, ValueEdge, ActionEdge, ActionNode};
use super::c_simple;
use super::c_simple::Ty;
use petgraph::visit::EdgeRef;
use petgraph::graph::{Graph, NodeIndex, EdgeIndex, Edges, EdgeReference};
use petgraph::{EdgeDirection, Direction, Directed};

fn is_debug() -> bool {
    cfg!(feature = "trace_log")
}

/// This constructs SimpleCAST from an instance of RadecoFunction.
pub fn recover_simple_ast(rfn: &RadecoFunction) -> SimpleCAST {
    let mut builder = CASTBuilder::new(rfn);
    // Recover values
    let data_graph = CASTDataMap::recover_data(rfn, &mut builder.ast);
    builder.datamap = data_graph;
    builder.declare_vars();
    // Recover control flow graph
    builder.cfg_from_blocks(builder.ssa.entry_node().unwrap(), &mut HashSet::new());
    builder.insert_jumps();
    builder.ast
}

// CASTBuilder constructs SimpleCAST from RadecoFunction
struct CASTBuilder<'a> {
    ast: SimpleCAST,
    // NodeIndex of SimpleCAST
    last_action: NodeIndex,
    rfn: &'a RadecoFunction,
    // SSA of RadecoFunction
    ssa: &'a SSAStorage,
    action_map: HashMap<NodeIndex, NodeIndex>,
    datamap: CASTDataMap<'a>,
}

impl<'a> CASTBuilder<'a> {
    fn new(rfn: &'a RadecoFunction) -> CASTBuilder {
        let ast = SimpleCAST::new(rfn.name.as_ref());
        CASTBuilder {
            last_action: ast.entry,
            ast: ast,
            rfn: rfn,
            ssa: rfn.ssa(),
            action_map: HashMap::new(),
            datamap: CASTDataMap::new(rfn),
        }
    }

    // XXX For debugging
    fn dummy_goto(&mut self) -> NodeIndex {
        self.last_action = self.ast.dummy_goto(self.last_action);
        self.last_action
    }

    // XXX For debugging
    fn dummy_action(&mut self, s: String) -> NodeIndex {
        self.last_action = self.ast.dummy(self.last_action, s);
        self.last_action
    }

    // Register, immidiate values are already declared in
    // prepare_regs, prepare_consts
    fn declare_vars(&mut self) {
        // TODO declare local variables
    }

    fn assign(&mut self, dst: NodeIndex, src: NodeIndex) -> NodeIndex {
        self.last_action = self.ast.assign(dst, src, self.last_action);
        self.last_action
    }

    fn call_action(&mut self, func: &str) -> NodeIndex {
        self.last_action = self.ast.call_func(func, &[], self.last_action, None);
        self.last_action
    }

    fn addr_str(&self, node: NodeIndex) -> String {
        self.ssa.address(node)
            .map(|a| format!("{}", a)).unwrap_or("unknown".to_string())
    }

    fn recover_action(&mut self, node: NodeIndex) -> NodeIndex {
        assert!(self.is_recover_action(node));
        let op = self.ssa.opcode(node).unwrap_or(MOpcode::OpInvalid);
        radeco_trace!("CASTBuilder::recover {:?} @ {:?}", op, node);
        match op {
            MOpcode::OpCall => {
                // TODO Add proper argument, require prototype from RadecoFunction
                let ret = self.call_action("func");
                if is_debug() {
                    let addr = self.addr_str(node);
                    let ops_dbg = self.ssa.operands_of(node);
                    self.ast.debug_info_at(ret, format!("Call {:?} @ {}", ops_dbg, addr));
                }
                ret
            }
            MOpcode::OpStore => {
                let ops = self.ssa.operands_of(node);
                let dst = self.datamap.var_map.get(&ops[1]).map(|&x| {
                    self.ast.derefed_node(x).unwrap_or(x)
                });
                let src = self.datamap.var_map.get(&ops[2]).cloned();
                let ret = if let (Some(d), Some(s)) = (dst, src) {
                    self.assign(d, s)
                } else {
                    self.dummy_action(format!("{:?} @ {:?}", op, node))
                };
                if is_debug() {
                    let addr = self.addr_str(node);
                    self.ast.debug_info_at(ret, format!("*({:?}) = {:?} @ {}", dst, src, addr));
                }
                ret
            }
            _ => unreachable!(),
        }
    }

    fn is_recover_action(&self, node: NodeIndex) -> bool {
        let op = self.ssa.opcode(node).unwrap_or(MOpcode::OpInvalid);
        match op {
            MOpcode::OpCall | MOpcode::OpStore => true,
            _ => false,
        }
    }

    fn get_block_addr(&self, block: NodeIndex) -> Option<MAddress> {
        match self.ssa.g[block] {
                NodeData::BasicBlock(addr, _) => Some(addr),
                _ => None,
        }
    }

    fn gen_label(&self, block: NodeIndex) -> String {
        if let Some(addr) = self.get_block_addr(block) {
            format!("addr_{:}", addr).to_string()
        } else {
            "addr_unknown".to_string()
        }
    }

    // ssa_node: SSA NodeIndex for goto statement
    // succ: SSA NodeIndex for destination node
    fn handle_goto(&mut self, ssa_node: NodeIndex, succ: NodeIndex) {
        radeco_trace!("CASTBuilder::handle goto");
        let ast_node = self.action_map.get(&ssa_node)
            .cloned().expect("The node should be added to action_map");
        let succ_node = self.action_map.get(&succ)
            .cloned().expect("This should not be None");
        let label = self.gen_label(succ);
        let goto_node = self.ast.insert_goto_before(ast_node, succ_node, &label);
        if is_debug() {
            let addr = self.addr_str(ssa_node);
            self.ast.debug_info_at(goto_node, format!("JMP {:?} @ {}", succ_node, addr));
        }
    }

    // ssa_node: SSA NodeIndex for if statement
    // selector: SSA NodeIndex for condition expression
    // true_node: SSA NodeIndex for if-then block
    fn handle_if(&mut self, ssa_node: NodeIndex, selector: NodeIndex, true_node: NodeIndex) {
        radeco_trace!("CASTBuilder::handle_if");
        let ast_node = self.action_map.get(&ssa_node)
            .cloned().expect("The node should be added to action_map");
        // Add goto statement as `if then` node
        let goto_node = {
            let dst_node = self.action_map.get(&true_node)
                .cloned().expect("This should not be None");
            // Edge from `unknown` will be removed later.
            let unknown = self.ast.unknown;
            let label = self.gen_label(true_node);
            self.ast.add_goto(dst_node, &label, unknown)
        };
        // Add condition node to if statement
        let cond = self.datamap.var_map.get(&selector).cloned().unwrap_or(self.ast.unknown);
        let if_node = self.ast.conditional_insert(cond, goto_node, None, ast_node);
        if is_debug() {
            let addr = self.addr_str(ssa_node);
            self.ast.debug_info_at(goto_node, format!("IF JMP {:?} @ {}", if_node, addr));
        }
    }

    // Insert goto, if statements
    fn insert_jumps(&mut self) {
        let mut last = None;
        let entry_node = entry_node_err!(self.ssa);
        for cur_node in self.ssa.inorder_walk() {
            if cur_node == entry_node {
                continue;
            }
            if last.is_some() && self.ssa.is_block(cur_node) {
                if let Some(succ) = self.ssa.unconditional_block(cur_node) {
                    if let Some(selector) = self.ssa.selector_in(cur_node) {
                        // TODO
                        radeco_trace!("CASTBuilder::insert_jumps INDIRET JMP");
                    } else {
                        self.handle_goto(cur_node, succ);
                    }
                } else if let Some(blk_cond_info) = self.ssa.conditional_blocks(cur_node) {
                    if let Some(selector) = self.ssa.selector_in(cur_node) {
                        self.handle_if(cur_node, selector, blk_cond_info.true_side);
                    } else {
                        radeco_warn!("block with conditional successors has no selector {:?}", cur_node);
                    }
                } else {
                    unreachable!();
                }
            } else if self.ssa.is_block(cur_node) {
                last = Some(cur_node);
            }
        }
    }

    fn cfg_from_nodes(&mut self, block: NodeIndex) {
        let nodes = self.ssa.nodes_in(block);
        for node in nodes {
            if self.is_recover_action(node) {
                let n = self.recover_action(node);
                self.action_map.insert(node, n);
            }
        }
    }

    fn cfg_from_blocks(&mut self, block: NodeIndex, visited: &mut HashSet<NodeIndex>) {
        if visited.contains(&block) {
            return;
        }
        visited.insert(block);
        let next_blocks = self.ssa.next_blocks(block);
        for blk in next_blocks {
            let n = self.dummy_goto();
            self.action_map.insert(blk, n);
            self.cfg_from_nodes(blk);
            self.cfg_from_blocks(blk, visited);
        }
    }
}

struct CASTDataMap<'a> {
    rfn: &'a RadecoFunction,
    ssa: &'a SSAStorage,
    // Hashmap from node of SSAStorage to one of self.data_graph
    // a map from node of data_graph to one of SimpleCAST's value
    pub var_map: HashMap<NodeIndex, NodeIndex>,
    // a map from node of data_graph to one of SimpleCAST's register
    pub reg_map: HashMap<String, NodeIndex>,
    pub const_nodes: HashSet<NodeIndex>,
    seen: HashSet<NodeIndex>,
}

impl<'a> CASTDataMap<'a> {
    fn new(rfn: &'a RadecoFunction) -> CASTDataMap<'a> {
        CASTDataMap {
            ssa: rfn.ssa(),
            rfn: rfn,
            var_map: HashMap::new(),
            reg_map: HashMap::new(),
            const_nodes: HashSet::new(),
            seen: HashSet::new(),
        }
    }

    // Returns data map from SSAStorage's NodeIndex to SimpleCAST's NodeIndex
    fn recover_data(rfn: &'a RadecoFunction, ast: &mut SimpleCAST) -> Self {
        let mut s = Self::new(rfn);
        s.prepare_consts(ast);
        s.prepare_regs(ast);
        for node in s.ssa.inorder_walk() {
            if s.ssa.is_phi(node) {
                s.handle_phi(node);
            } else if s.ssa.is_expr(node) {
                s.update_values(node, ast);
            }
        }
        s
    }

    fn handle_binop(&mut self, ret_node: NodeIndex, ops: Vec<NodeIndex>,
                    expr: c_simple::Expr, ast: &mut SimpleCAST) {
        assert!(ops.len() == 2);
        let ops_mapped = ops.iter()
            .map(|op| self.var_map.get(op).map(|n| *n).unwrap_or(ast.unknown))
            .collect::<Vec<_>>();
        let expr_node = ast.expr(ops_mapped.as_slice(), expr.clone());
        radeco_trace!("Add {:?} to {:?}, Operator: {:?}", ret_node, expr_node, expr);
        self.var_map.insert(ret_node, expr_node);
    }

    fn handle_uniop(&mut self, ret_node: NodeIndex, op: NodeIndex,
                    expr: c_simple::Expr, ast: &mut SimpleCAST) {
        if let Some(&n) = self.var_map.get(&op) {
            let expr_node = ast.expr(&[n], expr);
            self.var_map.insert(ret_node, expr_node);
        } else {
            radeco_warn!("Operand not found: {:?}", op);
        }
    }

    fn handle_cast(&mut self, ret_node: NodeIndex, op: NodeIndex,
                    expr: c_simple::Expr, ast: &mut SimpleCAST) {
        if self.const_nodes.contains(&op) {
            let ast_node = self.var_map.get(&op).cloned().unwrap_or(ast.unknown);
            self.var_map.insert(ret_node, ast_node);
        } else {
            self.handle_uniop(ret_node, op, expr, ast);
        }
    }

    fn deref(&self, node: NodeIndex, ast: &mut SimpleCAST) -> NodeIndex {
        radeco_trace!("DeRef {:?}", node);
        let n = self.var_map.get(&node).cloned().unwrap_or(ast.unknown);
        ast.deref(n)
    }

    fn handle_phi(&mut self, node: NodeIndex) {
        assert!(self.ssa.is_phi(node));
        radeco_trace!("CASTBuilder::handle_phi {:?}", node);
        let ops = self.ssa.operands_of(node);
        // Take first available/mappable node of SimpleCAST's node from phi node
        if let Some(&head) = ops.into_iter()
           .filter_map(|n| self.var_map.get(&n)).next() {
           self.var_map.insert(node, head);
        }
    }

    fn type_from_str(type_str: &str) -> Option<Ty> {
        // TODO More types
        match type_str {
            "int" => Some(Ty::new(c_simple::BTy::Int, true, 0)),
            _ => None,
        }
    }

    fn update_values(&mut self, ret_node: NodeIndex, ast: &mut SimpleCAST) {
        assert!(self.ssa.is_expr(ret_node));
        radeco_trace!("CASTBuilder::update_values {:?}", ret_node);
        if self.seen.contains(&ret_node) {
            return;
        }
        self.seen.insert(ret_node);
        if let Some(bindings) = self.rfn.local_at(ret_node) {
            // TODO add type
            let type_info = Self::type_from_str(&bindings[0].type_str);
            let ast_node = ast.var(bindings[0].name(), type_info);
            self.var_map.insert(ret_node, ast_node);
            return;
        }
        let ops = self.ssa.operands_of(ret_node);

        radeco_trace!("CASTBuilder::update_values opcode: {:?}", self.ssa.opcode(ret_node));
        match self.ssa.opcode(ret_node).unwrap_or(MOpcode::OpInvalid) {
            MOpcode::OpStore => {
                assert!(ops.len() == 3);
                // Variables do not need Deref
                if self.rfn.local_at(ops[1]).is_none() {
                    self.deref(ops[1], ast);
                }
            }
            MOpcode::OpLoad => {
                // Variables do not need Deref
                if self.rfn.local_at(ops[1]).is_none() {
                    let deref_node = self.deref(ops[1], ast);
                    self.var_map.insert(ret_node, deref_node);
                } else {
                    let ast_node = *self.var_map.get(&ops[1]).expect("This can not be `None`");
                    self.var_map.insert(ret_node, ast_node);
                }
            }
            MOpcode::OpAdd => self.handle_binop(ret_node, ops, c_simple::Expr::Add, ast),
            MOpcode::OpAnd => self.handle_binop(ret_node, ops, c_simple::Expr::And, ast),
            MOpcode::OpDiv => self.handle_binop(ret_node, ops, c_simple::Expr::Div, ast),
            MOpcode::OpEq => self.handle_binop(ret_node, ops, c_simple::Expr::Eq, ast),
            MOpcode::OpGt => self.handle_binop(ret_node, ops, c_simple::Expr::Gt, ast),
            // XXX Shl might be wrong operator
            MOpcode::OpLsl => self.handle_binop(ret_node, ops, c_simple::Expr::Shl, ast),
            // XXX Shr might be wrong operator
            MOpcode::OpLsr => self.handle_binop(ret_node, ops, c_simple::Expr::Shr, ast),
            MOpcode::OpLt => self.handle_binop(ret_node, ops, c_simple::Expr::Lt, ast),
            MOpcode::OpMod => self.handle_binop(ret_node, ops, c_simple::Expr::Mod, ast),
            MOpcode::OpMul => self.handle_binop(ret_node, ops, c_simple::Expr::Mul, ast),
            // TODO Add `Narrow` info
            MOpcode::OpNarrow(size) => self.handle_cast(ret_node, ops[0],
                                                         c_simple::Expr::Cast(size as usize), ast),
            MOpcode::OpNot => self.handle_uniop(ret_node, ops[0], c_simple::Expr::Not, ast),
            MOpcode::OpOr => self.handle_binop(ret_node, ops, c_simple::Expr::Or, ast),
            MOpcode::OpRol => unimplemented!(),
            MOpcode::OpRor => unimplemented!(),
            // TODO Add `SignExt`
            MOpcode::OpSignExt(size) => self.handle_cast(ret_node, ops[0],
                                                          c_simple::Expr::Cast(size as usize), ast),
            MOpcode::OpSub => self.handle_binop(ret_node, ops, c_simple::Expr::Sub, ast),
            MOpcode::OpXor => self.handle_binop(ret_node, ops, c_simple::Expr::Xor, ast),
            // TODO Add `ZeroExt`
            MOpcode::OpZeroExt(size) => self.handle_cast(ret_node, ops[0],
                                                          c_simple::Expr::Cast(size as usize), ast),
            MOpcode::OpCall => {
                self.update_data_graph_by_call(ret_node);
            },
            _ => {},
        }
    }
    fn update_data_graph_by_call(&mut self, call_node: NodeIndex) {
        radeco_trace!("CASTBuilder::update_data_graph_by_call {:?}", call_node);
        let reg_map = utils::call_rets(call_node, self.ssa);
        for (idx, (node, vt)) in reg_map.into_iter() {
            let name = self.ssa.regfile.get_name(idx).unwrap_or("mem").to_string();
            // TODO Add data dependencies for return values or registers
        }
    }

    fn prepare_consts(&mut self, ast: &mut SimpleCAST) {
        for (&val, &node) in self.ssa.constants.iter() {
            if let Ok(n) = self.ssa.node_data(node) {
                // TODO add type
                let ast_node = ast.constant(&val.to_string(), None);
                self.const_nodes.insert(node);
                self.var_map.insert(node, ast_node);
            } else {
                radeco_warn!("Invalid constant");
            }
        }
    }

    fn prepare_regs(&mut self, ast: &mut SimpleCAST) {
        for walk_node in self.ssa.inorder_walk() {
            // TODO avoid unwrap
            let reg_state = self.ssa.registers_in(walk_node);
            if reg_state.is_none() {
                continue;
            }
            let reg_map = utils::register_state_info(reg_state.unwrap(), self.ssa);
            for (idx, (node, vt)) in reg_map.into_iter() {
                let name = self.ssa.regfile.get_name(idx).unwrap_or("mem").to_string();
                // XXX SimpleCAST::constant may not be proper method for registering regs.
                let ast_node = ast.constant(&name, None);
                radeco_trace!("Add register {:?}", node);
                self.var_map.insert(node, ast_node);
                // XXX Maybe not needed
                self.reg_map.insert(name, ast_node);
            }
        }
    }
}
