//! File docs stay as doc comments.

// A comment above the imports.
use std::collections::HashMap;
use std::fmt::Write;

const HELP: &str = "line one \
    continues here\n\
    and here";

// ----------------------------------------------------------------
// A section header between items
// ----------------------------------------------------------------
struct Point {
    x: i32,
    y: i32,
}

impl Point {
    // a comment between methods
    fn new(x: i32, y: i32) -> Self {
        Point { x, y }
    }

    /// Doc comment on a method.
    fn sum(&self) -> i32 {
        self.x + self.y // end-of-line comment
    }
}

fn classify(n: i32) -> &'static str {
    match n {
        // zero is special
        0 => "zero",
        1 => "one", // just one

        // everything else
        _ => {
            // inside an arm
            "many"
        }
    }
}

fn main() {
    let p = Point::new(2, 3);

    // blank line above is kept
    let total = p.sum();
    let mut counts: HashMap<&str, i32> = HashMap::new();
    for n in [0, 1, 5] {
        // inside a loop
        *counts.entry(classify(n)).or_insert(0) += 1;
    }
    let mut s = String::new();
    write!(s, "{}", total).unwrap();
    let ok = matches!(total, 1..=10 | 20);
    let data = vec![(1, 2), (3, 4)];
    let m: HashMap<_, _> = data.into_iter().collect();
    if let Some(v) = m.get(&1) {
        println!("got {}", v);
    } else if let Some(v) = m.get(&3) {
        println!("other {}", v);
    }
    println!("{} {} {} {}", HELP, s, ok, counts.len());
    // trailing comment at the end of main
}
// last comment in the file
