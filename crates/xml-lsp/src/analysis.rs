//! Analyses of an open document cached per version.
//!
//! Zed sends `documentHighlight`, `codeAction`, `hover`,
//! `linkedEditingRange`, `foldingRange` or `documentSymbol` requests on
//! every cursor move and after every change; most of them start from the
//! tag tree of the whole document. The tree is computed once per version
//! of the document and shared by the requests until the next change
//! ([`AnalysisCache::invalidate`], called by the document store).

use std::{collections::HashMap, ops::Range, sync::Arc};

use xml_core::tags::XmlTagTree;

/// Documents whose tree is kept at most (the open documents, usually few).
const MAX_DOCUMENTS: usize = 64;

#[derive(Default)]
pub(crate) struct AnalysisCache {
    trees: HashMap<String, (usize, Arc<XmlTagTree>)>,
    schema_links: HashMap<String, SchemaLinks>,
}

/// `(reference, target)` ranges of the ID/IDREF and key/keyref values of a
/// document, computed against one merged schema.
pub(crate) type LinkRanges = Vec<(Range<usize>, Range<usize>)>;

/// Links of a document version, valid for the schema they were computed
/// with (identified by its address: the schema store hands out the same
/// `Arc` until a schema file or a catalog changes).
struct SchemaLinks {
    length: usize,
    schema: usize,
    links: Arc<LinkRanges>,
}

impl AnalysisCache {
    /// Tag tree of the current version of the document `uri`.
    pub(crate) fn tree(&mut self, uri: &str, source: &str) -> Arc<XmlTagTree> {
        // The length guards against a document replaced without
        // invalidation.
        if let Some((length, tree)) = self.trees.get(uri)
            && *length == source.len()
        {
            return tree.clone();
        }
        if self.trees.len() >= MAX_DOCUMENTS {
            self.trees.clear();
        }
        let tree = Arc::new(XmlTagTree::parse(source));
        self.trees
            .insert(uri.to_owned(), (source.len(), tree.clone()));
        tree
    }

    /// Links of the document against the schema at address `schema`
    /// (`compute` runs when the version or the schema changed): definition
    /// and references would otherwise rescan the whole document each time.
    pub(crate) fn schema_links(
        &mut self,
        uri: &str,
        source: &str,
        schema: usize,
        compute: impl FnOnce() -> LinkRanges,
    ) -> Arc<LinkRanges> {
        if let Some(cached) = self.schema_links.get(uri)
            && cached.length == source.len()
            && cached.schema == schema
        {
            return cached.links.clone();
        }
        if self.schema_links.len() >= MAX_DOCUMENTS {
            self.schema_links.clear();
        }
        let links = Arc::new(compute());
        self.schema_links.insert(
            uri.to_owned(),
            SchemaLinks {
                length: source.len(),
                schema,
                links: links.clone(),
            },
        );
        links
    }

    /// Forgets the analyses of `uri` (changed or closed).
    pub(crate) fn invalidate(&mut self, uri: &str) {
        self.trees.remove(uri);
        self.schema_links.remove(uri);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuses_the_tree_until_the_document_changes() {
        let mut cache = AnalysisCache::default();
        let first = cache.tree("file:///a.xml", "<a><b/></a>");
        let again = cache.tree("file:///a.xml", "<a><b/></a>");
        assert!(Arc::ptr_eq(&first, &again));
        cache.invalidate("file:///a.xml");
        let changed = cache.tree("file:///a.xml", "<a><c/></a>");
        assert!(!Arc::ptr_eq(&first, &changed));
        assert_eq!(changed.elements().len(), 2);
        // A different length is detected even without invalidation.
        let longer = cache.tree("file:///a.xml", "<a><cd/></a>");
        assert!(!Arc::ptr_eq(&changed, &longer));
    }

    #[test]
    fn reuses_schema_links_until_the_document_or_schema_changes() {
        let mut cache = AnalysisCache::default();
        let mut runs = 0;
        let mut links = |cache: &mut AnalysisCache, source: &str, schema: usize| {
            cache.schema_links("file:///a.xml", source, schema, || {
                runs += 1;
                vec![(0..1, 2..3)]
            })
        };
        let first = links(&mut cache, "<a/>", 1);
        let again = links(&mut cache, "<a/>", 1);
        assert!(Arc::ptr_eq(&first, &again));
        links(&mut cache, "<a/>", 2);
        links(&mut cache, "<ab/>", 2);
        cache.invalidate("file:///a.xml");
        links(&mut cache, "<ab/>", 2);
        assert_eq!(runs, 4);
    }
}
