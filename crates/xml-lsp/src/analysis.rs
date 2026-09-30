//! Analyses of an open document cached per version.
//!
//! Zed sends `documentHighlight`, `codeAction`, `hover`,
//! `linkedEditingRange`, `foldingRange` or `documentSymbol` requests on
//! every cursor move and after every change; most of them start from the
//! tag tree of the whole document. The tree is computed once per version
//! of the document and shared by the requests until the next change
//! ([`AnalysisCache::invalidate`], called by the document store).

use std::{collections::HashMap, sync::Arc};

use xml_core::tags::XmlTagTree;

/// Documents whose tree is kept at most (the open documents, usually few).
const MAX_DOCUMENTS: usize = 64;

#[derive(Default)]
pub(crate) struct AnalysisCache {
    trees: HashMap<String, (usize, Arc<XmlTagTree>)>,
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

    /// Forgets the analyses of `uri` (changed or closed).
    pub(crate) fn invalidate(&mut self, uri: &str) {
        self.trees.remove(uri);
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
}
