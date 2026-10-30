use std::collections::HashMap;

#[derive(Debug)]
enum Shape {
    Circle { r: f64 },
    Rect { w: f64, h: f64 },
    Dot,
}

fn area(s: &Shape) -> f64 {
    match s {
        Shape::Circle { r } => 3.14 * r * r,
        Shape::Rect { w, h } if *w > 0.0 => {
            let a = w * h;
            a
        }
        Shape::Rect { .. } => 0.0,
        Shape::Dot => 0.0,
    }
}

fn parse_pair(text: &str) -> Option<(i32, i32)> {
    let (a, b) = text.split_once(',')?;
    let Ok(x) = a.trim().parse::<i32>() else {
        return None;
    };
    let y: i32 = b.trim().parse().ok()?;
    Some((x, y))
}

fn first_even(v: &[i32]) -> Option<i32> {
    for &x in v {
        if x % 2 == 0 {
            return Some(x);
        }
    }
    None
}

fn main() {
    let shapes = vec![Shape::Circle { r: 1.0 }, Shape::Rect { w: 2.0, h: 3.0 }, Shape::Dot];
    let total: f64 = shapes.iter().map(|s| area(s)).sum();
    println!("total area {:.2}", total);

    let squares: Vec<i32> = (1..=5).map(|n| {
        let sq = n * n;
        sq + 0
    }).filter(|x| x % 2 == 1).collect();
    println!("{:?}", squares);

    if let Some((x, y)) = parse_pair("3, 4") {
        println!("pair {} {}", x, y + 1);
    } else {
        println!("no pair");
    }
    println!("{:?}", parse_pair("nope"));

    let mut stack = vec![1, 2, 3];
    while let Some(top) = stack.pop() {
        println!("top {}", top * 10);
    }

    let mut counts: HashMap<char, usize> = HashMap::new();
    for c in "hello world".chars().filter(|c| c.is_alphabetic()) {
        *counts.entry(c).or_insert(0) += 1;
    }
    let mut keys: Vec<_> = counts.iter().collect();
    keys.sort();
    println!("{:?}", keys);

    'outer: for i in 0..5 {
        for j in 0..5 {
            if i * j == 6 {
                println!("found {} {}", i, j);
                break 'outer;
            }
        }
    }

    let label = match first_even(&[1, 3, 8, 5]) {
        Some(n) if n > 5 => format!("big {}", n),
        Some(n) => format!("small {}", n),
        None => "none".to_string(),
    };
    println!("{}", label);

    let offset = 100;
    let add = |a: i32, b: i32| a + b + offset;
    println!("{}", add(1, 2));
    let p = Shape::Rect { w: 1.5, h: 2.0 };
    println!("{:?} {}", p, area(&p) as i64);
}
