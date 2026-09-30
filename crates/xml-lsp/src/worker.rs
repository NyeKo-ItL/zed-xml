//! Diagnostics computed off the request loop.
//!
//! Validation can take long (a large schema set, a big document): it runs
//! on a dedicated thread so that the request loop keeps answering
//! completion, hover or formatting while the user types. The worker owns a
//! replica of the analysis state (an [`XmlLanguageServer`] without worker:
//! open documents, settings, workspace folders, catalogs and its own schema
//! and DTD caches), kept up to date by the [`Job`]s the request loop sends
//! in order; [`XmlLanguageServer::diagnostics`] stays the single place
//! computing diagnostics.
//!
//! Only the latest snapshot of a document is validated: jobs are absorbed
//! before each validation (several changes coalesce into one), and a result
//! whose document or settings changed while it was computed is dropped
//! instead of being published (the document is validated again). A panic
//! while validating one document is logged and does not stop the worker.

use std::{
    collections::HashMap,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread::{self, JoinHandle},
    time::Instant,
};

use serde_json::{Value, json};

use crate::{
    XmlLanguageServer, dispatch::panic_message, is_xsd_uri, positions::PositionEncoding,
    settings::Settings,
};

/// State change or validation request sent by the request loop.
pub(crate) enum Job {
    /// Opened or changed document (`text`), or closed document (`None`).
    Document {
        uri: String,
        version: Option<i64>,
        text: Option<String>,
    },
    /// New effective settings.
    Settings(Box<Settings>),
    /// `workspace/didChangeWorkspaceFolders` parameters.
    WorkspaceFolders(Value),
    /// Validate every open document again (settings, catalogs, folders).
    ValidateAll,
    /// Acknowledged once every job sent before has been processed and its
    /// diagnostics published.
    #[cfg(test)]
    Flush(Sender<()>),
}

/// Handle of the diagnostics thread; dropping it stops the thread.
pub(crate) struct DiagnosticsWorker {
    jobs: Option<Sender<Job>>,
    thread: Option<JoinHandle<()>>,
}

impl DiagnosticsWorker {
    /// Starts the worker on `replica`; `publish` sends a
    /// `textDocument/publishDiagnostics` notification and returns `false`
    /// once the client connection is closed.
    pub(crate) fn spawn(
        replica: XmlLanguageServer,
        encoding: PositionEncoding,
        publish: impl Fn(Value) -> bool + Send + 'static,
    ) -> Self {
        let (jobs, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("xml-lsp-diagnostics".to_owned())
            .spawn(move || {
                encoding.install();
                Worker::new(replica, receiver, publish).run();
            })
            .map_err(|error| eprintln!("xml-lsp: cannot start the diagnostics thread: {error}"))
            .ok();
        Self {
            jobs: thread.as_ref().map(|_| jobs),
            thread,
        }
    }

    pub(crate) fn send(&self, job: Job) {
        if let Some(jobs) = &self.jobs
            && jobs.send(job).is_err()
        {
            eprintln!("xml-lsp: the diagnostics thread has stopped");
        }
    }

    /// Waits until the jobs sent so far are processed (at most `timeout`).
    #[cfg(test)]
    pub(crate) fn flush(&self, timeout: std::time::Duration) -> bool {
        let (done, wait) = mpsc::channel();
        self.send(Job::Flush(done));
        wait.recv_timeout(timeout).is_ok()
    }
}

impl Drop for DiagnosticsWorker {
    fn drop(&mut self) {
        // Closing the channel ends the worker loop.
        self.jobs = None;
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            eprintln!("xml-lsp: the diagnostics thread panicked");
        }
    }
}

struct Worker<P> {
    replica: XmlLanguageServer,
    jobs: Receiver<Job>,
    publish: P,
    /// Documents to validate, in the order of their changes.
    dirty: Vec<String>,
    /// Schemas whose change requires validating the documents using them.
    changed_schemas: Vec<String>,
    /// Versions reported by the client.
    versions: HashMap<String, i64>,
    /// Incremented on each change of a document, to detect stale results.
    generations: HashMap<String, u64>,
    /// Incremented on each settings or workspace change.
    epoch: u64,
    flushes: Vec<Sender<()>>,
    /// Set when the request loop has gone.
    disconnected: bool,
}

impl<P: Fn(Value) -> bool> Worker<P> {
    fn new(replica: XmlLanguageServer, jobs: Receiver<Job>, publish: P) -> Self {
        Self {
            replica,
            jobs,
            publish,
            dirty: Vec::new(),
            changed_schemas: Vec::new(),
            versions: HashMap::new(),
            generations: HashMap::new(),
            epoch: 0,
            flushes: Vec::new(),
            disconnected: false,
        }
    }

    fn run(mut self) {
        while !self.disconnected {
            let deadline = self.deadline();
            let job = match deadline {
                None => self.jobs.recv().ok(),
                Some(deadline) => {
                    match self
                        .jobs
                        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    {
                        Ok(job) => Some(job),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            };
            match job {
                Some(job) => self.apply(job),
                None if deadline.is_none() => break,
                None => {}
            }
            self.absorb();
            if self.due() {
                self.validate_dirty();
            }
            if self.dirty.is_empty() && self.changed_schemas.is_empty() {
                for flush in self.flushes.drain(..) {
                    let _ = flush.send(());
                }
            }
        }
    }

    /// When the pending validations are due (`None` when there are none).
    fn deadline(&self) -> Option<Instant> {
        (!self.dirty.is_empty() || !self.changed_schemas.is_empty()).then(Instant::now)
    }

    fn due(&self) -> bool {
        !self.flushes.is_empty()
            || self
                .deadline()
                .is_some_and(|deadline| deadline <= Instant::now())
    }

    /// Applies the jobs already queued, without waiting.
    fn absorb(&mut self) {
        loop {
            match self.jobs.try_recv() {
                Ok(job) => self.apply(job),
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.disconnected = true;
                    return;
                }
            }
        }
    }

    fn apply(&mut self, job: Job) {
        match job {
            Job::Document { uri, version, text } => {
                *self.generations.entry(uri.clone()).or_default() += 1;
                match version {
                    Some(version) => self.versions.insert(uri.clone(), version),
                    None => self.versions.remove(&uri),
                };
                match text {
                    Some(text) => {
                        self.replica.documents.insert(uri.clone(), text);
                        if is_xsd_uri(&uri) && !self.changed_schemas.contains(&uri) {
                            self.changed_schemas.push(uri.clone());
                        }
                        self.mark(uri);
                    }
                    None => {
                        self.replica.documents.remove(&uri);
                        self.versions.remove(&uri);
                        self.dirty.retain(|dirty| *dirty != uri);
                        (self.publish)(json!({"uri": uri, "diagnostics": []}));
                    }
                }
            }
            Job::Settings(settings) => {
                self.epoch += 1;
                self.replica.settings = *settings;
                self.replica.update_catalogs();
            }
            Job::WorkspaceFolders(params) => {
                self.epoch += 1;
                self.replica.workspace.change_folders(&params);
                self.replica.update_catalogs();
            }
            Job::ValidateAll => self.mark_all(),
            #[cfg(test)]
            Job::Flush(done) => self.flushes.push(done),
        }
    }

    fn mark(&mut self, uri: String) {
        if !self.dirty.contains(&uri) {
            self.dirty.push(uri);
        }
    }

    fn mark_all(&mut self) {
        let mut uris = self.replica.documents.keys().cloned().collect::<Vec<_>>();
        uris.sort();
        for uri in uris {
            self.mark(uri);
        }
    }

    /// Validates the pending documents, publishing only up-to-date results.
    fn validate_dirty(&mut self) {
        if !self.replica.catalogs.is_empty() && self.replica.catalogs.refresh() {
            self.mark_all();
        }
        for schema in std::mem::take(&mut self.changed_schemas) {
            let mut dependents = self
                .replica
                .documents
                .keys()
                .filter(|uri| **uri != schema && self.replica.references_schema(uri, &schema))
                .cloned()
                .collect::<Vec<_>>();
            dependents.sort();
            for uri in dependents {
                self.mark(uri);
            }
        }
        while !self.dirty.is_empty() {
            let uri = self.dirty.remove(0);
            let Some(source) = self.replica.documents.get(&uri).cloned() else {
                continue;
            };
            let generation = self.generations.get(&uri).copied();
            let epoch = self.epoch;
            let result = catch_unwind(AssertUnwindSafe(|| self.replica.diagnostics(&uri, &source)));
            self.absorb();
            let mut params = match result {
                Ok(params) => params,
                Err(payload) => {
                    eprintln!(
                        "xml-lsp: internal error while validating {uri}: {}",
                        panic_message(payload.as_ref())
                    );
                    continue;
                }
            };
            if self.generations.get(&uri).copied() != generation || self.epoch != epoch {
                // Changed meanwhile: the newer snapshot is validated instead.
                self.mark(uri);
                continue;
            }
            if let Some(version) = self.versions.get(&uri) {
                params["version"] = json!(version);
            }
            if !(self.publish)(params) {
                self.disconnected = true;
                return;
            }
            if self.disconnected {
                return;
            }
        }
    }
}
