use std::collections::HashMap;

use client_classification::ast;
use criterion::{criterion_group, criterion_main, Criterion};
use dhcproto::v4::UnknownOption;
use pest::Parser;

// use client_classification::{one, two};

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function(
        "substring(mac, 0, 6) == '001122' && option[61].hex == 'some_client_id'",
        |b| {
            b.iter(|| {
                let mut options = HashMap::new();
                options.insert(
                    61.into(),
                    UnknownOption::new(61.into(), b"some_client_id".to_vec()),
                );

                let chaddr = "001122334455";

                let tokens = ast::PredicateParser::parse(
                    ast::Rule::expr,
                    "substring(pkt4.mac, 0, 6) == '001122' and option[61].hex == 'some_client_id'",
                )
                .unwrap();

                client_classification::ast::eval_ast(
                    client_classification::ast::build_ast(tokens).unwrap(),
                    chaddr,
                    &options,
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
                let chaddr = "001122334455";
                let mut options = HashMap::new();
                options.insert(
                    61.into(),
                    UnknownOption::new(61.into(), b"some_client_id".to_vec()),
                );

                client_classification::ast::eval_ast(ast.clone(), chaddr, &options).unwrap()
            })
        },
    );
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
