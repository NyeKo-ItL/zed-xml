//! Benchmarks of XSD parsing, merging, validation and completion on a
//! generated schema set and large instance documents.
//!
//! `cargo bench -p xsd-core` (add `-- --quick` for a fast run).

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use xsd_core::{
    complete_elements, merge_schemas, parse_xsd, validate_document, validate_document_located,
};

/// A schema with a `catalog` of `book`s plus `extra` unrelated global
/// components, as in large industry schema sets.
fn schema(extra: usize) -> String {
    let mut text = String::from(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="catalog"><xs:complexType><xs:sequence>
    <xs:element ref="book" minOccurs="0" maxOccurs="unbounded"/>
  </xs:sequence></xs:complexType></xs:element>
  <xs:element name="book"><xs:complexType><xs:sequence>
    <xs:element name="title" type="xs:string"/>
    <xs:element name="author" type="xs:string" maxOccurs="unbounded"/>
    <xs:element name="price" type="xs:decimal"/>
    <xs:element name="format" type="Format"/>
  </xs:sequence>
  <xs:attribute name="id" type="xs:ID" use="required"/>
  <xs:attribute name="lang" type="xs:language"/>
  </xs:complexType></xs:element>
  <xs:simpleType name="Format"><xs:restriction base="xs:string">
    <xs:enumeration value="paperback"/><xs:enumeration value="hardcover"/><xs:enumeration value="ebook"/>
  </xs:restriction></xs:simpleType>
"#,
    );
    for index in 0..extra {
        text.push_str(&format!(
            "  <xs:element name=\"extra{index}\"><xs:complexType><xs:sequence><xs:element name=\"value{index}\" type=\"xs:string\"/></xs:sequence><xs:attribute name=\"code{index}\" type=\"xs:token\"/></xs:complexType></xs:element>\n"
        ));
    }
    text.push_str("</xs:schema>\n");
    text
}

/// An instance of about `size` bytes, with one invalid book every 100.
fn instance(size: usize) -> String {
    let mut text = String::from("<catalog>\n");
    let mut index = 0;
    while text.len() < size {
        let format = if index % 100 == 99 { "scroll" } else { "ebook" };
        text.push_str(&format!(
            "  <book id=\"b{index}\" lang=\"en\"><title>Title {index}</title><author>A</author><author>B</author><price>{index}.5</price><format>{format}</format></book>\n"
        ));
        index += 1;
    }
    text.push_str("</catalog>\n");
    text
}

fn benches(c: &mut Criterion) {
    let mut group = c.benchmark_group("xsd-core");
    group.sample_size(10);
    let source = schema(2000);
    group.throughput(Throughput::Bytes(source.len() as u64));
    group.bench_function("parse_xsd/2000-components", |b| {
        b.iter(|| parse_xsd(black_box(&source)))
    });
    let parsed = parse_xsd(&source).expect("the generated schema should parse");
    group.bench_function("merge_schemas/4x2000-components", |b| {
        b.iter(|| merge_schemas((0..4).map(|_| parsed.clone())))
    });
    for size in [100_000, 1_000_000] {
        let document = instance(size);
        group.throughput(Throughput::Bytes(document.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("validate_document_located", size),
            &document,
            |b, document| b.iter(|| validate_document_located(black_box(document), &parsed)),
        );
        group.bench_with_input(
            BenchmarkId::new("validate_document", size),
            &document,
            |b, document| b.iter(|| validate_document(black_box(document), &parsed)),
        );
    }
    let open = instance(100_000);
    let typing = format!("{}  <", open.strip_suffix("</catalog>\n").unwrap_or(&open));
    group.bench_function("complete_elements/100000", |b| {
        b.iter(|| complete_elements(black_box(&typing), typing.len(), &parsed))
    });
    group.finish();
}

criterion_group!(xsd_core_benches, benches);
criterion_main!(xsd_core_benches);
