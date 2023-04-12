use std::collections::{HashMap, HashSet};

use client_classification::{ast, Args};
use criterion::{criterion_group, criterion_main, Criterion};
use dhcproto::v4::{self, UnknownOption};
use pest::Parser;

// use client_classification::{one, two};

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function(
        "substring(mac, 0, 6) == '001122' && option[61].hex == 'some_client_id'",
        |b| {
            b.iter(|| {
                let mut opts = HashMap::new();
                opts.insert(
                    61.into(),
                    UnknownOption::new(61.into(), b"some_client_id".to_vec()),
                );

                let chaddr = "001122334455".to_owned();

                let tokens = ast::PredicateParser::parse(
                    ast::Rule::expr,
                    "substring(pkt4.mac, 0, 6) == '001122' and option[61].hex == 'some_client_id'",
                )
                .unwrap();

                let args = Args {
                    chaddr,
                    opts,
                    msg: &v4::Message::default(),
                    deps: HashSet::new(),
                };
                client_classification::eval(
                    &client_classification::ast::build_ast(tokens).unwrap(),
                    &args,
                )
                .unwrap()
            })
        },
    );
    c.bench_function(
        "just eval: substring(pkt4.mac, 0, 6) == '001122' && option[61].hex == 'some_client_id'",
        |b| {
            let tokens = ast::PredicateParser::parse(
                ast::Rule::expr,
                "substring(pkt4.mac, 0, 6) == '001122' and option[61].hex == 'some_client_id'",
            )
            .unwrap();

            let ast = client_classification::ast::build_ast(tokens).unwrap();

            b.iter(move || {
                let chaddr = "001122334455".to_owned();
                let mut opts = HashMap::new();
                opts.insert(
                    61.into(),
                    UnknownOption::new(61.into(), b"some_client_id".to_vec()),
                );
                let args = Args {
                    chaddr,
                    opts,
                    msg: &v4::Message::default(),
                    deps: HashSet::new(),
                };
                client_classification::eval(&ast, &args).unwrap()
            })
        },
    );
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
