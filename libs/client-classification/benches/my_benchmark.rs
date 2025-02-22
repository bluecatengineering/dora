use std::collections::{HashMap, HashSet};

use client_classification::{Args, PacketDetails, ast};
use criterion::{Criterion, criterion_group, criterion_main};
use dhcproto::v4::{self, UnknownOption};
use pest::Parser;

// use client_classification::{one, two};

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function(
        "substring('foobar, 0, 6) == 'foo' && option[61].hex == 'some_client_id'",
        |b| {
            b.iter(|| {
                let mut opts = HashMap::new();
                opts.insert(
                    61.into(),
                    UnknownOption::new(61.into(), b"some_client_id".to_vec()),
                );

                let chaddr = &hex::decode("DEADBEEF").unwrap();
                let tokens = ast::PredicateParser::parse(
                    ast::Rule::expr,
                    "substring('foobar', 0, 3) == 'foo' and option[61].hex == 'some_client_id'",
                )
                .unwrap();

                let args = Args {
                    chaddr,
                    opts,
                    msg: &v4::Message::default(),
                    member: HashSet::new(),
                    pkt: PacketDetails::default(),
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
        "just eval: substring('foobar', 0, 6) == 'foo' && option[61].hex == 'some_client_id'",
        |b| {
            let tokens = ast::PredicateParser::parse(
                ast::Rule::expr,
                "substring('foobar', 0, 6) == 'foo' and option[61].hex == 'some_client_id'",
            )
            .unwrap();

            let ast = client_classification::ast::build_ast(tokens).unwrap();

            b.iter(move || {
                let chaddr = &hex::decode("DEADBEEF").unwrap();
                let mut opts = HashMap::new();
                opts.insert(
                    61.into(),
                    UnknownOption::new(61.into(), b"some_client_id".to_vec()),
                );
                let args = Args {
                    chaddr,
                    opts,
                    msg: &v4::Message::default(),
                    member: HashSet::new(),
                    pkt: PacketDetails::default(),
                };
                client_classification::eval(&ast, &args).unwrap()
            })
        },
    );
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
