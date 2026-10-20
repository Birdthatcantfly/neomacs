//! Invocation-owned bytecode publication barrier. No runtime state or cache.
//! The coordinator joins all Rayon workers before validation/publication.
//! Children share immutable job references and write independent private targets;
//! no Lisp value, Context, or mutator-owned state crosses these worker threads.
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Immutable plan for one independent compiler process and destination.
/// Safe to borrow across Rayon workers: fields are owned paths, not Lisp state;
/// PlannedWave construction rejects canonical destination collisions.
#[derive(Debug)]
pub(crate) struct PendingOutput {
    source: PathBuf,
    canonical: PathBuf,
    pending: PathBuf,
}

impl PendingOutput {
    pub(crate) fn source(&self) -> &Path {
        &self.source
    }

    /// Install after preload refresh and before the final batch compiler.
    /// Unexpected nested targets fail instead of mutating canonical inputs.
    pub(crate) fn redirect_args(&self, args: &mut Vec<OsString>) -> io::Result<()> {
        let insertion = args
            .len()
            .checked_sub(3)
            .ok_or_else(|| io::Error::other("expected final -f FUNCTION SOURCE"))?;
        if args[insertion] != "-f" || args.last().is_none_or(|arg| arg != self.source.as_os_str()) {
            return Err(io::Error::other("unexpected batch compiler argv shape"));
        }
        let source = crate::elisp_string_literal(&self.source);
        let pending = crate::elisp_string_literal(&self.pending);
        let expression = format!(
            "(progn (require 'bytecomp) (setq byte-compile-dest-file-function \
             (lambda (file) (if (equal (expand-file-name file) {source}) \
             {pending} (error \"Unexpected nested byte-compile target: %s\" file)))))"
        );
        args.splice(
            insertion..insertion,
            [OsString::from("--eval"), expression.into()],
        );
        Ok(())
    }
}

/// Single invocation/coordinator owner of staging and its immutable job table.
/// Workers borrow jobs only; the coordinator must join them before consuming
/// this owner for validation, retention, or publication. No shared mutable state.
#[must_use = "join children and validate, or explicitly retain failed outputs"]
pub(crate) struct PlannedWave {
    directory: tempfile::TempDir,
    jobs: Vec<PendingOutput>,
}

impl PlannedWave {
    pub(crate) fn create(root: &Path, sources: &[PathBuf]) -> io::Result<Self> {
        let root = root.canonicalize()?;
        // Same filesystem: publication renames preserve raw bytes/mode/mtime.
        // This private directory is never appended to load-path.
        let directory = tempfile::Builder::new()
            .prefix(".neomacs-bytecode-wave-")
            .tempdir_in(&root)?;
        let mut seen = BTreeSet::new();
        let mut jobs = Vec::with_capacity(sources.len());
        for source in sources {
            if !fs::symlink_metadata(source)?.file_type().is_file() {
                return Err(io::Error::other(
                    "bytecode source must be regular, not a symlink",
                ));
            }
            let source = source.canonicalize()?;
            let relative = source.strip_prefix(&root).map_err(io::Error::other)?;
            if source.extension().is_none_or(|suffix| suffix != "el") {
                return Err(io::Error::other("bytecode source must end in .el"));
            }
            let canonical = source.with_extension("elc");
            if !seen.insert(canonical.clone()) {
                return Err(io::Error::other("duplicate bytecode wave destination"));
            }
            let pending = directory
                .path()
                .join("pending")
                .join(relative)
                .with_extension("elc");
            fs::create_dir_all(
                pending
                    .parent()
                    .ok_or_else(|| io::Error::other("missing output parent"))?,
            )?;
            jobs.push(PendingOutput {
                source,
                canonical,
                pending,
            });
        }
        Ok(Self { directory, jobs })
    }

    pub(crate) fn jobs(&self) -> &[PendingOutput] {
        &self.jobs
    }

    pub(crate) fn retain(self) -> PathBuf {
        self.directory.keep()
    }

    /// Called only after the entire worker pool has joined successfully.
    /// This checks artifact presence/type/size, not semantic bytecode validity;
    /// normal compiler status and fresh-build dump/load sanity remain required.
    pub(crate) fn validate(self) -> io::Result<ValidatedWave> {
        let validation = self.jobs.iter().try_for_each(|job| {
            let metadata = fs::symlink_metadata(&job.pending)?;
            if !metadata.file_type().is_file() || metadata.len() == 0 {
                return Err(io::Error::other(format!(
                    "missing/empty/nonregular output {}",
                    job.pending.display()
                )));
            }
            Ok(())
        });
        if let Err(error) = validation {
            let retained = self.retain();
            return Err(io::Error::other(format!(
                "wave validation failed: {error}; retained {}",
                retained.display()
            )));
        }
        Ok(ValidatedWave(self))
    }
}

/// Coordinator-owned presence/type/size validation capability after worker join.
/// No worker may still be mutating the paths; no runtime or Lisp state is stored.
#[must_use = "validated outputs must be published before starting the next wave"]
pub(crate) struct ValidatedWave(PlannedWave);

impl ValidatedWave {
    /// No compiler is still running. Each rename is atomic; this is not a
    /// filesystem transaction. A failure retains the remaining products,
    /// reports exact already-published targets, and forbids later waves.
    pub(crate) fn publish(self) -> io::Result<PublishedWave> {
        let plan = self.0;
        let mut published = Vec::new();
        for index in 0..plan.jobs.len() {
            let result = fs::rename(&plan.jobs[index].pending, &plan.jobs[index].canonical);
            if let Err(error) = result {
                let retained = plan.retain();
                return Err(io::Error::other(format!(
                    "wave publish failed: {error}; already published {published:?}; retained {}",
                    retained.display()
                )));
            }
            published.push(plan.jobs[index].canonical.clone());
        }
        Ok(PublishedWave(plan.directory))
    }
}

/// Coordinator-owned cleanup capability after every canonical rename succeeds.
/// Invocation-owned filesystem storage only; no worker references or Lisp state.
#[must_use = "staging cleanup must be explicit"]
pub(crate) struct PublishedWave(tempfile::TempDir);

impl PublishedWave {
    pub(crate) fn finish(self) -> io::Result<()> {
        self.0.close()
    }
}

#[cfg(test)]
#[path = "compile_wave/tests/publication_test.rs"]
mod tests;
