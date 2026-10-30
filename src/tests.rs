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

// ---------------- extract function ----------------

/// Compile and run a Rust source file with rustc, returning stdout.
fn run_rust(src: &str, tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rn-extract-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.rs");
    std::fs::write(&file, src).unwrap();
    let exe = dir.join("prog");
    let out = std::process::Command::new("rustc").arg("--edition=2021").arg("-o").arg(&exe).arg(&file).output().expect("rustc");
    assert!(out.status.success(), "rustc failed:\n{}\n---\n{src}", String::from_utf8_lossy(&out.stderr));
    let run = std::process::Command::new(&exe).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8(run.stdout).unwrap()
}

/// Group statements `range` of main's top-level chain (with the value nodes feeding them) and extract.
fn extract_stmts(src: &str, range: std::ops::Range<usize>, name: &str) -> Result<(String, crate::extract::Plan), String> {
    let mut p = parse_program(src).unwrap();
    let path = p.functions().into_iter().find(|(_, f)| f.name == "main").unwrap().0;
    let f = p.function_mut(&path).unwrap();
    let chain = top_chain(f);
    let mut ids: Vec<NodeId> = chain[range.clone()].to_vec();
    // pull in value nodes that only feed the chosen statements (like box-selecting them)
    let mut stack = ids.clone();
    while let Some(id) = stack.pop() {
        for pin in 0..f.nodes[&id].kind.data_ins().len() {
            if let Some((s, _)) = f.data_source(id, pin) {
                if f.nodes[&s].scope.is_none() && !f.nodes[&s].kind.is_exec() && !ids.contains(&s) {
                    ids.push(s);
                    stack.push(s);
                }
            }
        }
    }
    let g = f.collapse(&ids, name).unwrap();
    let mut plan = crate::extract::plan(f, g)?;
    plan.name = name.to_string();
    crate::extract::apply(&mut p, &path, &plan)?;
    let out = generate(&p);
    assert!(out.error.is_none(), "{:?}", out.error);
    Ok((out.code, plan))
}

const AREA: &str = r#"fn main() {
    let width = 4;
    let height = 5;
    let area = width * height;
    println!("area = {}", area);
    let mut total = 0;
    for i in 1..=area {
        total += i;
    }
    println!("total = {}", total);
}
"#;

#[test]
fn extract_returns_two_values() {
    let (code, plan) = extract_stmts(AREA, 0..2, "dimensions").unwrap();
    assert_eq!(plan.signature(), "fn dimensions() -> (i32, i32)");
    assert!(code.contains("let dimensions_out = dimensions();"), "{code}");
    assert!(code.contains("let width = dimensions_out.0;"), "{code}");
    assert!(code.contains("fn dimensions() -> (i32, i32) {"), "{code}");
    assert_eq!(run_rust(&code, "two"), run_rust(AREA, "two-orig"));
}

#[test]
fn extract_with_params_and_one_return() {
    // `let area = width * height;` uses two outside values and produces one
    let (code, plan) = extract_stmts(AREA, 2..3, "compute_area").unwrap();
    assert_eq!(plan.signature(), "fn compute_area(width: i32, height: i32) -> i32");
    assert!(code.contains("let area = compute_area(width, height);"), "{code}");
    assert_eq!(run_rust(&code, "one"), run_rust(AREA, "one-orig"));
}

#[test]
fn extract_mutation_becomes_mut_reference() {
    // the for loop changes `total`, which lives outside the group
    let (code, plan) = extract_stmts(AREA, 5..6, "sum_up").unwrap();
    assert_eq!(plan.signature(), "fn sum_up(area: i32, total: &mut i32)");
    assert!(code.contains("sum_up(area, &mut total);"), "{code}");
    assert!(code.contains("*total += i;"), "{code}");
    assert_eq!(run_rust(&code, "mut"), run_rust(AREA, "mut-orig"));
}

#[test]
fn extract_refuses_return() {
    let src = "fn main() {\n    let x = 3;\n    if x > 2 {\n        return;\n    }\n    println!(\"{}\", x);\n}\n";
    let err = extract_stmts(src, 1..2, "check").unwrap_err();
    assert!(err.contains("return"), "{err}");
}

#[test]
fn extract_refuses_break_of_outside_loop() {
    let src = "fn main() {\n    let mut i = 0;\n    loop {\n        i += 1;\n        if i > 3 {\n            break;\n        }\n    }\n    println!(\"{}\", i);\n}\n";
    // group only the `if` inside the loop body is not reachable via top_chain; group the loop: allowed
    let (code, _) = extract_stmts(src, 1..2, "count_up").unwrap();
    assert_eq!(run_rust(&code, "loop"), run_rust(src, "loop-orig"));
}

// ---------------- asking the compiler for types ----------------

const PARTS: &str = r#"fn main() {
    let text = "a,b,c";
    let parts = text.split(',').collect::<Vec<_>>();
    let count = parts.len();
    let label = format!("{} parts", count);
    println!("{}", label);
}
"#;

/// Group main's statements `range`, ask the compiler for the types, extract, and return the new code.
fn extract_with_compiler(src: &str, file: &std::path::Path, range: std::ops::Range<usize>, name: &str) -> (String, crate::extract::Plan) {
    std::fs::write(file, src).unwrap();
    let mut p = parse_program(src).unwrap();
    let path = p.functions().into_iter().find(|(_, f)| f.name == "main").unwrap().0;
    let f = p.function_mut(&path).unwrap();
    let chain = top_chain(f);
    let g = f.collapse(&chain[range], name).unwrap();
    let mut plan = crate::extract::plan(f, g).unwrap();
    plan.name = name.to_string();
    let (code, targets) = crate::extract::probe_code(&p, &path, &plan).unwrap();
    let types = crate::compiler::probe_types(file, &code, targets.len()).unwrap();
    for (t, ty) in targets.iter().zip(types) {
        let ty = ty.expect("compiler gave a type");
        match *t {
            crate::extract::ProbeTarget::Param(k) => plan.params[k].ty = ty,
            crate::extract::ProbeTarget::Output(k) => plan.outputs[k].ty = ty,
        }
    }
    crate::extract::apply(&mut p, &path, &plan).unwrap();
    (generate(&p).code, plan)
}

#[test]
fn compiler_types_with_rustc() {
    let dir = std::env::temp_dir().join(format!("rn-probe-rustc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.rs");
    // `parts` comes from a turbofish collect: no way to guess, the compiler must say Vec<&str>
    let mut plan_before = parse_program(PARTS).unwrap();
    let path = plan_before.functions().into_iter().find(|(_, f)| f.name == "main").unwrap().0;
    let f = plan_before.function_mut(&path).unwrap();
    let chain = top_chain(f);
    let g = f.collapse(&chain[2..3], "x").unwrap();
    assert_eq!(crate::extract::plan(f, g).unwrap().params[0].ty, "", "guessing should fail here");

    let (code, plan) = extract_with_compiler(PARTS, &file, 2..3, "count_parts");
    assert_eq!(plan.signature(), "fn count_parts(parts: Vec<&str>) -> usize");
    assert_eq!(run_rust(&code, "probe-rustc"), run_rust(PARTS, "probe-rustc-orig"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn compiler_types_with_cargo() {
    let dir = std::env::temp_dir().join(format!("rn-probe-cargo-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"probe_test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n").unwrap();
    let file = dir.join("src").join("main.rs");
    // returning `parts` (a Vec<&str>) and taking `text` (&str)
    let (code, plan) = extract_with_compiler(PARTS, &file, 1..2, "split_parts");
    assert_eq!(plan.signature(), "fn split_parts(text: &str) -> Vec<&str>");
    assert_eq!(run_rust(&code, "probe-cargo"), run_rust(PARTS, "probe-cargo-orig"));
    // the user's project folder was not modified
    assert_eq!(std::fs::read_to_string(&file).unwrap(), PARTS);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn compiler_found_texts() {
    use crate::extract::type_from_found;
    assert_eq!(type_from_found("integer").unwrap(), "i32");
    assert_eq!(type_from_found("floating-point number").unwrap(), "f64");
    assert_eq!(type_from_found("`Vec<{integer}>`").unwrap(), "Vec<i32>");
    assert_eq!(type_from_found("struct `HashMap<char, usize>`").unwrap(), "HashMap<char, usize>");
    assert!(type_from_found("closure").is_err());
}
