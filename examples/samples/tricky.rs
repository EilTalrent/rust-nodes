use std::collections::HashMap;

const LIMIT: i32 = 10;

fn precedence(a: i32, b: i32, c: i32) -> i32 {
    let x = (a + b) * c;
    let y = a - (b - c);
    let z = -(a * b) + !0 - c / (a + 1);
    let w = (a < b) == (b < c);
    if w && (x > 3 || y < 0) {
        return x;
    }
    let x = x + 1;
    x * y + z
}

fn early(v: &[i32]) -> Option<i32> {
    let mut best = None;
    for &x in v {
        if x < 0 {
            return None;
        }
        best = Some(x);
    }
    best
}

fn words(text: &str) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    for w in text.split_whitespace() {
        *map.entry(w.to_lowercase()).or_insert(0) += 1;
    }
    map
}

fn main() {
    let mut i = 0;
    while i < LIMIT {
        i += 1;
        if i % 2 == 0 {
            continue;
        }
        let msg = format!("odd {}", i);
        println!("{msg}");
    }
    let mut stack = vec![3, 1, 2];
    while let Some(top) = stack.pop() {
        println!("popped {}", top);
    }
    let arr = [10, 20, 30];
    println!("{} {}", arr[1], arr.len());
    let t = (1, "two");
    println!("{} {}", t.0, t.1);
    println!("{}", precedence(1, 2, 3));
    println!("{:?} {:?}", early(&[1, 2, 3]), early(&[1, -2]));
    let counts = words("a b A c b a");
    let mut keys: Vec<_> = counts.keys().cloned().collect();
    keys.sort();
    for k in &keys {
        println!("{} => {}", k, counts[k]);
    }
    let s = String::from("hi");
    let r = &s;
    let n = r.len() as i32;
    println!("{}", n * -1);
}
