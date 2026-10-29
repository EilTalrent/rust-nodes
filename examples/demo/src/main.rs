//! A small program to try in the node editor.

#[derive(Debug, Clone)]
struct Counter {
    count: u32,
    step: u32,
}

impl Counter {
    fn new(step: u32) -> Self {
        Counter { count: 0, step }
    }

    fn tick(&mut self) -> u32 {
        self.count += self.step;
        self.count
    }
}

fn fizzbuzz(n: u32) -> String {
    if n % 15 == 0 {
        "FizzBuzz".to_string()
    } else if n % 3 == 0 {
        "Fizz".to_string()
    } else if n % 5 == 0 {
        "Buzz".to_string()
    } else {
        n.to_string()
    }
}

fn sum_of_squares(values: &[i64]) -> i64 {
    let mut total = 0;
    for v in values {
        total += v * v;
    }
    total
}

fn main() {
    let name = "world";
    println!("Hello, {}!", name);

    for i in 1..=15 {
        println!("{}", fizzbuzz(i));
    }

    let mut counter = Counter::new(3);
    let mut ticks = 0;
    while counter.count < 20 {
        counter.tick();
        ticks += 1;
    }
    println!("{:?} after {} ticks", counter, ticks);

    let numbers = vec![1, 2, 3, 4];
    let doubled: Vec<i64> = numbers.iter().map(|x| x * 2).collect();
    println!("sum of squares = {}", sum_of_squares(&doubled));

    let mut n = 27u64;
    let mut steps = 0;
    loop {
        if n == 1 {
            break;
        }
        n = if n % 2 == 0 { n / 2 } else { 3 * n + 1 };
        steps += 1;
    }
    println!("collatz(27) takes {} steps", steps);

    match steps {
        0 => println!("none"),
        s if s > 100 => println!("long"),
        _ => println!("short"),
    }
}
