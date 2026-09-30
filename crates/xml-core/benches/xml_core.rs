//! Benchmarks of the XML building blocks on large generated documents:
//! parsing with diagnostics, well-formedness, tag tree and formatting.
//!
//! `cargo bench -p xml-core` (add `-- --quick` for a fast run).

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xml_core::{FormatOptions, format_xml_with, parse_xml, tags::XmlTagTree, wellformed};

/// A document of about `size` bytes: namespaced records with attributes,
/// text, comments, CDATA and non-ASCII characters, on CRLF lines.
fn document(size: usize) -> String {
    let mut text = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<catalog xmlns=\"urn:bench\" xmlns:x=\"urn:x\">\r\n",
    );
    let mut index = 0;
    while text.len() < size {
        text.push_str(&format!(
            "  <book id=\"b{index}\" x:lang=\"fr\">\r\n    <title>Titre n°{index} &amp; suite</title>\r\n    <!-- note {index} -->\r\n    <author>Émile {index}</author><author>Zoë</author>\r\n    <price currency=\"EUR\">{}.99</price>\r\n    <summary><![CDATA[<p>{index}</p>]]></summary>\r\n  </book>\r\n",
            index % 100
        ));
        index += 1;
    }
    text.push_str("</catalog>\r\n");
    text
}

fn benches(c: &mut Criterion) {
    let mut group = c.benchmark_group("xml-core");
    group.sample_size(10);
    for size in [100_000, 1_000_000] {
        let source = document(size);
        group.throughput(Throughput::Bytes(source.len() as u64));
        group.bench_with_input(BenchmarkId::new("parse_xml", size), &source, |b, source| {
            b.iter(|| parse_xml(black_box(source)))
        });
        group.bench_with_input(
            BenchmarkId::new("check_well_formedness", size),
            &source,
            |b, source| b.iter(|| wellformed::check_well_formedness(black_box(source))),
        );
        group.bench_with_input(BenchmarkId::new("tag_tree", size), &source, |b, source| {
            b.iter(|| XmlTagTree::parse(black_box(source)))
        });
        let options = FormatOptions::default();
        group.bench_with_input(BenchmarkId::new("format", size), &source, |b, source| {
            b.iter(|| format_xml_with(black_box(source), &options))
        });
    }
    group.finish();
}

criterion_group!(xml_core_benches, benches);
criterion_main!(xml_core_benches);
