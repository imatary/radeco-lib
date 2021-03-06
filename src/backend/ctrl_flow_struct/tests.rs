use super::condition::*;
use super::*;

#[derive(Default, Debug)]
struct StringAst {
    var_count: u64,
}

impl AstContext for StringAst {
    type Block = String;
    type Variable = String;
    type Condition = String;

    fn mk_fresh_var(&mut self) -> String {
        let ret = format!("i_{}", self.var_count);
        self.var_count += 1;
        ret
    }

    fn mk_cond_equals(&mut self, var: &String, val: u64) -> String {
        format!("{} == {}", var, val)
    }

    fn mk_var_assign(&mut self, var: &String, val: u64) -> String {
        format!("{} = {}", var, val)
    }

    fn mk_break(&mut self) -> String {
        "break".to_owned()
    }
}

#[test]
fn nmg_example() {
    let cstore = ConditionStorage::new();
    let cctx = ConditionContext::new(&cstore);

    let mut graph = StableDiGraph::new();
    let entry = graph.add_node(cnode());
    let c1 = graph.add_node(cnode());
    let c2 = graph.add_node(cnode());
    let c3 = graph.add_node(cnode());
    let b1 = graph.add_node(cnode());
    let b2 = graph.add_node(cnode());
    let d1 = graph.add_node(cnode());
    let d2 = graph.add_node(cnode());
    let d3 = graph.add_node(cnode());
    let n1 = graph.add_node(node("n1"));
    let n2 = graph.add_node(node("n2"));
    let n3 = graph.add_node(node("n3"));
    let n4 = graph.add_node(node("n4"));
    let n5 = graph.add_node(node("n5"));
    let n6 = graph.add_node(node("n6"));
    let n7 = graph.add_node(node("n7"));
    let n8 = graph.add_node(node("n8"));
    let n9 = graph.add_node(node("n9"));

    #[allow(non_snake_case)]
    let c_A = cond_s(&cctx, "A");
    let c_c1 = cond_s(&cctx, "c1");
    let c_c2 = cond_s(&cctx, "c2");
    let c_c3 = cond_s(&cctx, "c3");
    let c_b1 = cond_s(&cctx, "b1");
    let c_b2 = cond_s(&cctx, "b2");
    let c_d1 = cond_s(&cctx, "d1");
    let c_d2 = cond_s(&cctx, "d2");
    let c_d3 = cond_s(&cctx, "d3");

    graph.add_edge(entry, c1, Some(c_A));
    graph.add_edge(entry, b1, neg_c(&cctx, c_A));
    // R1
    graph.add_edge(c1, n1, Some(c_c1));
    graph.add_edge(n1, c1, None);
    graph.add_edge(c1, c2, neg_c(&cctx, c_c1));
    graph.add_edge(c2, n2, Some(c_c2));
    graph.add_edge(n2, n9, None);
    graph.add_edge(c2, n3, neg_c(&cctx, c_c2));
    graph.add_edge(n3, c3, None);
    graph.add_edge(c3, c1, Some(c_c3));
    graph.add_edge(c3, n9, neg_c(&cctx, c_c3));
    // R2
    graph.add_edge(b1, b2, Some(c_b1));
    graph.add_edge(b2, n6, Some(c_b2));
    graph.add_edge(n6, n7, None);
    graph.add_edge(n7, d1, None);
    graph.add_edge(b2, n5, neg_c(&cctx, c_b2));
    graph.add_edge(n5, n7, None);
    graph.add_edge(b1, n4, neg_c(&cctx, c_b1));
    graph.add_edge(n4, n5, None);
    // R3
    graph.add_edge(d1, d3, Some(c_d1));
    graph.add_edge(d3, n8, Some(c_d3));
    graph.add_edge(n8, d1, None);
    graph.add_edge(d3, n9, neg_c(&cctx, c_d3));
    graph.add_edge(d1, d2, neg_c(&cctx, c_d1));
    graph.add_edge(d2, n8, Some(c_d2));
    graph.add_edge(d2, n9, neg_c(&cctx, c_d2));

    let actx = StringAst::default();
    let cfg = ControlFlowGraph {
        graph,
        entry,
        cctx,
        actx,
    };
    let ast = cfg.structure_whole();
    println!("{:#?}", ast);
}

#[test]
fn abnormal_entries() {
    let cstore = ConditionStorage::new();
    let cctx = ConditionContext::new(&cstore);

    let mut graph = StableDiGraph::new();
    let entry = graph.add_node(cnode());
    let n1 = graph.add_node(cnode());
    let n2 = graph.add_node(cnode());
    let n3 = graph.add_node(cnode());
    let n4 = graph.add_node(cnode());
    let n5 = graph.add_node(cnode());
    let f = graph.add_node(node("f"));
    let l1 = graph.add_node(cnode());
    let l2 = graph.add_node(node("l2"));
    let l3 = graph.add_node(node("l3"));

    let c_e1 = cond_s(&cctx, "e1");
    let c_n1 = cond_s(&cctx, "n1");
    let c_n2 = cond_s(&cctx, "n2");
    let c_n3 = cond_s(&cctx, "n3");
    let c_n4 = cond_s(&cctx, "n4");
    let c_n5 = cond_s(&cctx, "n5");
    let c_l1 = cond_s(&cctx, "l1");

    graph.add_edge(entry, l1, Some(c_e1));
    graph.add_edge(entry, n1, neg_c(&cctx, c_e1));
    graph.add_edge(n1, n2, Some(c_n1));
    graph.add_edge(n2, n3, Some(c_n2));
    graph.add_edge(n3, n4, Some(c_n3));
    graph.add_edge(n4, n5, Some(c_n4));
    graph.add_edge(n5, f, Some(c_n5));
    // loop
    graph.add_edge(l1, l2, Some(c_l1));
    graph.add_edge(l2, l3, None);
    graph.add_edge(l3, l1, None);
    // loop exit
    graph.add_edge(l1, f, neg_c(&cctx, c_l1));
    // abnormal entries
    graph.add_edge(n1, l1, neg_c(&cctx, c_n1));
    graph.add_edge(n2, l2, neg_c(&cctx, c_n2));
    graph.add_edge(n3, l3, neg_c(&cctx, c_n3));
    graph.add_edge(n4, l2, neg_c(&cctx, c_n4));
    graph.add_edge(n5, l2, neg_c(&cctx, c_n5));

    let actx = StringAst::default();
    let cfg = ControlFlowGraph {
        graph,
        entry,
        cctx,
        actx,
    };
    let ast = cfg.structure_whole();
    println!("{:#?}", ast);
}

#[test]
fn abnormal_exits() {
    let cstore = ConditionStorage::new();
    let cctx = ConditionContext::new(&cstore);

    let mut graph = StableDiGraph::new();
    let entry = graph.add_node(cnode());
    let n1 = graph.add_node(node("n1"));
    let n2 = graph.add_node(node("n2"));
    let n3 = graph.add_node(node("n3"));
    let n4 = graph.add_node(node("n4"));
    let n5 = graph.add_node(node("n5"));
    let f = graph.add_node(node("f"));
    let l1 = graph.add_node(cnode());
    let l2 = graph.add_node(cnode());
    let l3 = graph.add_node(cnode());
    let l4 = graph.add_node(cnode());
    let l5 = graph.add_node(cnode());

    let c_e1 = cond_s(&cctx, "e1");
    let c_l1 = cond_s(&cctx, "l1");
    let c_l2 = cond_s(&cctx, "l2");
    let c_l3 = cond_s(&cctx, "l3");
    let c_l4 = cond_s(&cctx, "l4");
    let c_l5 = cond_s(&cctx, "l5");

    graph.add_edge(entry, l1, Some(c_e1));
    graph.add_edge(entry, n1, neg_c(&cctx, c_e1));
    graph.add_edge(n1, n2, None);
    graph.add_edge(n2, n3, None);
    graph.add_edge(n3, n4, None);
    graph.add_edge(n4, n5, None);
    graph.add_edge(n5, f, None);
    // loop
    graph.add_edge(l1, l2, Some(c_l1));
    graph.add_edge(l2, l3, Some(c_l2));
    graph.add_edge(l3, l4, Some(c_l3));
    graph.add_edge(l4, l5, Some(c_l4));
    graph.add_edge(l5, l1, Some(c_l5));
    // loop exit
    graph.add_edge(l1, f, neg_c(&cctx, c_l1));
    graph.add_edge(l4, f, neg_c(&cctx, c_l4));
    // abnormal exits
    graph.add_edge(l2, n2, neg_c(&cctx, c_l2));
    graph.add_edge(l3, n2, neg_c(&cctx, c_l3));
    graph.add_edge(l5, n5, neg_c(&cctx, c_l5));

    let actx = StringAst::default();
    let cfg = ControlFlowGraph {
        graph,
        entry,
        cctx,
        actx,
    };
    let ast = cfg.structure_whole();
    println!("{:#?}", ast);
}

#[test]
fn infinite_loop() {
    let cstore = ConditionStorage::new();
    let cctx = ConditionContext::new(&cstore);

    let mut graph = StableDiGraph::new();
    let entry = graph.add_node(node("entry"));
    let n1 = graph.add_node(node("n1"));

    graph.add_edge(entry, n1, None);
    graph.add_edge(n1, entry, None);

    let actx = StringAst::default();
    let cfg = ControlFlowGraph {
        graph,
        entry,
        cctx,
        actx,
    };
    let ast = cfg.structure_whole();
    println!("{:#?}", ast);
}

fn cond_s<'cd>(cctx: &ConditionContext<'cd, String>, c: &str) -> Condition<'cd, StringAst> {
    cctx.mk_simple(c.to_owned())
}

fn neg_c<'cd>(
    cctx: &ConditionContext<'cd, String>,
    c: Condition<'cd, StringAst>,
) -> Option<Condition<'cd, StringAst>> {
    Some(cctx.mk_not(c))
}

fn node(n: &str) -> CfgNode<'static, StringAst> {
    CfgNode::Code(AstNode::BasicBlock(n.to_owned()))
}

fn cnode() -> CfgNode<'static, StringAst> {
    CfgNode::Condition
}
