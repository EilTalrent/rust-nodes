//! Round-trip tests: Rust → graph → Rust must be valid, stable, and (checked
//! separately by compiling) behave the same.

use crate::codegen::generate;
use crate::graph::*;
use crate::parse::parse_program;

fn roundtrip(src: &str) -> String {
    let p = parse_program(src).expect("parses");
    let g = generate(&p);
    assert!(g.error.is_none(), "generated invalid Rust: {:?}\n{}", g.error, g.code);
    g.code
}

fn check_file(path: &str) {
    let src = std::fs::read_to_string(path).unwrap();
    let once = roundtrip(&src);
    let twice = roundtrip(&once);
    assert_eq!(once, twice, "round trip of {path} is not stable");
}

#[test]
fn demo_roundtrips() {
    check_file("examples/demo/src/main.rs");
}

#[test]
fn tricky_roundtrips() {
    check_file("examples/samples/tricky.rs");
}

#[test]
fn precedence_is_preserved() {
    let out = roundtrip("fn f(a: i32, b: i32, c: i32) -> i32 { (a + b) * c - (b - c) }");
    assert!(out.contains("(a + b) * c - (b - c)"), "{out}");
}

#[test]
fn shadowing_wires_to_previous_let() {
    let p = parse_program("fn f() { let x = 1; let x = x + 1; println!(\"{}\", x); }").unwrap();
    let f = p.functions()[0].1.clone();
    let lets: Vec<NodeId> =
        f.nodes.iter().filter(|(_, n)| matches!(n.kind, NodeKind::Let { .. })).map(|(id, _)| *id).collect();
    assert_eq!(lets.len(), 2);
    // the `+` node's first input comes from the first `let x`
    let plus = f.nodes.iter().find(|(_, n)| matches!(&n.kind, NodeKind::Binary { op } if op == "+")).unwrap().0;
    assert_eq!(f.data_source(*plus, 0), Some((lets[0], 0)));
}

#[test]
fn build_graph_by_hand() {
    // What the editor does: add nodes and wires, then generate code.
    let mut f = FnGraph::new("main".into(), "fn main()".into(), vec![]);
    let let_x = f.add(NodeKind::Let { name: "x".into(), mutable: false, ty: String::new() }, [0.0, 0.0]);
    let two = f.add(NodeKind::Literal { code: "2".into() }, [0.0, 0.0]);
    let three = f.add(NodeKind::Literal { code: "3".into() }, [0.0, 0.0]);
    let mul = f.add(NodeKind::Binary { op: "*".into() }, [0.0, 0.0]);
    let print = f.add(NodeKind::Eval, [0.0, 0.0]);
    let mac = f.add(NodeKind::Macro { name: "println".into(), args: 2, bracket: false }, [0.0, 0.0]);
    let fmt = f.add(NodeKind::Literal { code: "\"{}\"".into() }, [0.0, 0.0]);
    f.connect_exec(f.entry, 0, let_x);
    f.connect_exec(let_x, 0, print);
    f.connect_data(two, 0, mul, 0);
    f.connect_data(three, 0, mul, 1);
    f.connect_data(mul, 0, let_x, 0);
    f.connect_data(fmt, 0, mac, 0);
    f.connect_data(let_x, 0, mac, 1);
    f.connect_data(mac, 0, print, 0);
    let code = generate(&Program { items: vec![Item::Func(f)] }).code;
    assert_eq!(code, "fn main() {\n    let x = 2 * 3;\n    println!(\"{}\", x);\n}\n");
}

#[test]
fn unsupported_code_is_kept_raw() {
    let src = "fn f(o: Option<i32>) -> i32 { match o { Some(v) => v, None => 0 } }";
    let out = roundtrip(src);
    assert!(out.contains("match o"), "{out}");
}

#[test]
fn containers_roundtrip() {
    check_file("examples/samples/containers.rs");
}

#[test]
fn match_becomes_template_with_containers() {
    let p = parse_program("fn f(o: Option<i32>) -> i32 { match o { Some(v) => v * 2, None => 0 } }").unwrap();
    let f = p.functions()[0].1.clone();
    let (mid, m) = f.nodes.iter().find(|(_, n)| matches!(n.kind, NodeKind::Template { .. })).unwrap();
    let NodeKind::Template { code, holes } = &m.kind else { unreachable!() };
    assert_eq!(code, "match ⟦0⟧ {\n    Some(v) => ⟦1⟧,\n    None => ⟦2⟧,\n}");
    assert!(!holes[0].block && holes[1].block && holes[2].block);
    assert_eq!(holes[1].binders, vec!["v".to_string()]);
    // the scrutinee hole is wired to the parameter `o`
    assert_eq!(f.data_source(*mid, 0), Some((f.entry, 0)));
    // inside arm 1, `v * 2` uses the container's `v` output
    let be = f.block_entry(*mid, 1).unwrap();
    let mul = f.nodes.iter().find(|(_, n)| matches!(&n.kind, NodeKind::Binary { op } if op == "*")).unwrap();
    assert_eq!(mul.1.scope, Some((*mid, 1)));
    assert_eq!(f.data_source(*mul.0, 0), Some((be, 0)));
    // and it regenerates without braces
    let out = generate(&p).code;
    assert!(out.contains("Some(v) => v * 2,"), "{out}");
}

#[test]
fn closure_captures_outer_variable() {
    let out = roundtrip("fn f() -> i32 { let k = 3; let add = |a: i32| a + k; add(1) }");
    assert!(out.contains("let add = |a: i32| a + k;"), "{out}");
}

#[test]
fn deleting_a_container_deletes_its_inside() {
    let p = parse_program("fn f(o: Option<i32>) -> i32 { match o { Some(v) => v * 2, None => 0 } }").unwrap();
    let mut f = p.functions()[0].1.clone();
    let mid = *f.nodes.iter().find(|(_, n)| matches!(n.kind, NodeKind::Template { .. })).unwrap().0;
    f.remove(mid);
    assert!(f.nodes.values().all(|n| n.scope.is_none()), "inner nodes left behind");
}

fn main_of(p: &mut Program) -> &mut FnGraph {
    let path = p.functions().into_iter().find(|(_, f)| f.name == "main").unwrap().0;
    p.function_mut(&path).unwrap()
}

/// Top-level exec nodes after Entry, in order.
fn top_chain(f: &FnGraph) -> Vec<NodeId> {
    let mut out = vec![];
    let mut cur = f.exec_target(f.entry, 0);
    while let Some(id) = cur {
        out.push(id);
        cur = f.nodes[&id].kind.next_pin().and_then(|p| f.exec_target(id, p));
    }
    out
}

#[test]
fn grouping_does_not_change_code() {
    let src = std::fs::read_to_string("examples/samples/containers.rs").unwrap();
    let mut p = parse_program(&src).unwrap();
    let before = generate(&p).code;
    let f = main_of(&mut p);
    let chain = top_chain(f);
    // group statements 2..6 together with the value nodes feeding them
    let mut ids: Vec<NodeId> = chain[2..6].to_vec();
    for id in chain[2..6].iter() {
        for pin in 0..f.nodes[id].kind.data_ins().len() {
            if let Some((src, _)) = f.data_source(*id, pin) {
                if f.nodes[&src].scope.is_none() && !f.nodes[&src].kind.is_exec() {
                    ids.push(src);
                }
            }
        }
    }
    let g = f.collapse(&ids, "setup").expect("grouped");
    let inner = f.collapse(&chain[3..5], "inner").expect("nested group");
    assert_eq!(f.nodes[&inner].scope, Some((g, 0)));
    assert_eq!(f.rep(chain[3], None), Some(g));
    assert_eq!(f.rep(chain[3], Some((g, 0))), Some(inner));
    assert_eq!(generate(&p).code, before, "grouping changed the generated code");

    let f = main_of(&mut p);
    f.ungroup(inner);
    f.ungroup(g);
    assert!(f.nodes.values().all(|n| !matches!(n.kind, NodeKind::Group { .. })));
    assert!(chain.iter().all(|id| f.nodes[id].scope.is_none()));
    assert_eq!(generate(&p).code, before);
}

#[test]
fn entry_cannot_be_grouped() {
    let mut p = parse_program("fn main() { let a = 1; println!(\"{}\", a); }").unwrap();
    let f = main_of(&mut p);
    let entry = f.entry;
    assert!(f.collapse(&[entry], "x").is_none());
}

#[test]
fn groups_survive_editing_the_file_outside() {
    let dir = std::env::temp_dir().join(format!("rust-nodes-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.rs");
    std::fs::write(&file, "fn main() {\n    let a = 1;\n    let b = a + 2;\n    println!(\"{}\", b);\n}\n").unwrap();
    let mut p = crate::project::open(&file).unwrap().program;
    let f = main_of(&mut p);
    let chain = top_chain(f);
    f.collapse(&chain[0..2], "math").unwrap();
    crate::project::save(&file, &p).unwrap();

    // edit the .rs in a "text editor": add a line at the end
    let text = std::fs::read_to_string(&file).unwrap().replace("println!(\"{}\", b);", "println!(\"{}\", b);\n    println!(\"done\");");
    std::fs::write(&file, text).unwrap();

    let loaded = crate::project::open(&file).unwrap();
    assert!(loaded.note.contains("re-read"), "{}", loaded.note);
    let mut p2 = loaded.program;
    let f2 = main_of(&mut p2);
    let groups: Vec<NodeId> = f2.nodes.keys().copied().filter(|id| f2.is_group(*id)).collect();
    assert_eq!(groups.len(), 1);
    let members = f2.members(groups[0]);
    let lets = members.iter().filter(|id| matches!(f2.nodes[id].kind, NodeKind::Let { .. })).count();
    assert_eq!(lets, 2, "both lets should be back in the group");
    assert!(generate(&p2).code.contains("println!(\"done\")"));
    let _ = std::fs::remove_dir_all(&dir);
}
