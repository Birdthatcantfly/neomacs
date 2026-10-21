//! Invocation-owned bytecode publication barrier. No runtime state or cache.
//! The coordinator joins all Rayon workers before validation/publication.
//! Children share immutable job references and write independent private targets;
//! no Lisp value, Context, or mutator-owned state crosses these worker threads.
use std::collections::{BTreeMap, BTreeSet};
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

/// The Lisp program that lists each source's compile-time loads
/// (`require`s the compiler evaluates and autoloads it triggers).  Run with
/// the final dumped binary before compile-main; see the file's commentary.
pub(crate) const LOAD_SCAN_PROGRAM: &str = include_str!("../compile-deps.el");

/// Parse the scanner's "SOURCE<TAB>DEPENDENCY" lines into edges between
/// members of `sources`.  Edges to files outside the set (bytecode compiled by
/// an earlier stage, or no file at all) and self-edges are dropped.  The
/// scanner prints dependencies as true names, so they are matched through
/// canonical paths.
pub(crate) fn parse_load_dependencies(
    output: &str,
    sources: &[PathBuf],
) -> BTreeMap<PathBuf, BTreeSet<PathBuf>> {
    let by_canonical = sources
        .iter()
        .map(|source| (canonical_or_same(source), source.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut edges = BTreeMap::<PathBuf, BTreeSet<PathBuf>>::new();
    for line in output.lines() {
        let Some((source, dependency)) = line.split_once('\t') else {
            continue;
        };
        let (Some(source), Some(dependency)) = (
            by_canonical.get(&canonical_or_same(Path::new(source))),
            by_canonical.get(&canonical_or_same(Path::new(dependency))),
        ) else {
            continue;
        };
        if source != dependency {
            edges
                .entry(source.clone())
                .or_default()
                .insert(dependency.clone());
        }
    }
    edges
}

fn canonical_or_same(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Order `sources` into compile waves so a file compiles only after the
/// bytecode of every source it depends on has been published.
///
/// Each wave is a topological layer: a file's wave is one past the deepest
/// wave among its dependencies, so every wave runs fully in parallel and
/// sees exactly the bytecode of the waves before it.  The waves depend only
/// on the sources and the edges, never on process completion order.
///
/// Dependency cycles are broken deterministically: inside a strongly
/// connected component the members are ordered by path, and a member keeps
/// only its edges to earlier members.  It compiles against the bytecode of
/// the earlier members and the sources of the later ones.
pub(crate) fn dependency_waves(
    sources: Vec<PathBuf>,
    dependencies: &BTreeMap<PathBuf, BTreeSet<PathBuf>>,
) -> Vec<Vec<PathBuf>> {
    let nodes = sources.into_iter().collect::<BTreeSet<_>>();
    let index = nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (node, i))
        .collect::<BTreeMap<_, _>>();
    let edges = nodes
        .iter()
        .map(|node| {
            dependencies
                .get(node)
                .into_iter()
                .flatten()
                .filter_map(|dep| index.get(dep).copied())
                .filter(|dep| *dep != index[node])
                .collect::<BTreeSet<usize>>()
        })
        .collect::<Vec<_>>();

    let component = strongly_connected_components(&edges);
    // Nodes are numbered in path order, so "earlier member" is a smaller
    // index within the same component.
    let kept = |node: usize, dep: usize| component[node] != component[dep] || dep < node;

    let mut depth = vec![None::<usize>; edges.len()];
    for start in 0..edges.len() {
        // Iterative post-order over the kept (acyclic) edges.
        let mut stack = vec![(start, false)];
        while let Some((node, expanded)) = stack.pop() {
            if depth[node].is_some() {
                continue;
            }
            if expanded {
                let deepest = edges[node]
                    .iter()
                    .filter(|dep| kept(node, **dep))
                    .map(|dep| depth[*dep].expect("dependency depth computed first") + 1)
                    .max()
                    .unwrap_or(0);
                depth[node] = Some(deepest);
            } else {
                stack.push((node, true));
                for dep in edges[node].iter().filter(|dep| kept(node, **dep)) {
                    if depth[*dep].is_none() {
                        stack.push((*dep, false));
                    }
                }
            }
        }
    }

    let mut waves = Vec::<Vec<PathBuf>>::new();
    for (node, path) in nodes.into_iter().enumerate() {
        let wave = depth[node].expect("every node has a depth");
        if waves.len() <= wave {
            waves.resize_with(wave + 1, Vec::new);
        }
        waves[wave].push(path);
    }
    waves
}

/// Tarjan's algorithm, iterative.  Returns each node's component number.
fn strongly_connected_components(edges: &[BTreeSet<usize>]) -> Vec<usize> {
    const UNVISITED: usize = usize::MAX;
    let count = edges.len();
    let mut order = vec![UNVISITED; count];
    let mut low = vec![0; count];
    let mut on_stack = vec![false; count];
    let mut component = vec![UNVISITED; count];
    let mut stack = Vec::new();
    let mut next_order = 0;
    let mut next_component = 0;

    for root in 0..count {
        if order[root] != UNVISITED {
            continue;
        }
        // (node, iterator position over its sorted successors)
        let mut frames = vec![(root, 0usize)];
        order[root] = next_order;
        low[root] = next_order;
        next_order += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(frame) = frames.last_mut() {
            let node = frame.0;
            if let Some(&dep) = edges[node].iter().nth(frame.1) {
                frame.1 += 1;
                if order[dep] == UNVISITED {
                    order[dep] = next_order;
                    low[dep] = next_order;
                    next_order += 1;
                    stack.push(dep);
                    on_stack[dep] = true;
                    frames.push((dep, 0));
                } else if on_stack[dep] {
                    low[node] = low[node].min(order[dep]);
                }
                continue;
            }
            frames.pop();
            if let Some(&(parent, _)) = frames.last() {
                low[parent] = low[parent].min(low[node]);
            }
            if low[node] == order[node] {
                loop {
                    let member = stack.pop().expect("Tarjan stack holds the component");
                    on_stack[member] = false;
                    component[member] = next_component;
                    if member == node {
                        break;
                    }
                }
                next_component += 1;
            }
        }
    }
    component
}

#[cfg(test)]
#[path = "compile_wave/tests/publication_test.rs"]
mod tests;

#[cfg(test)]
#[path = "compile_wave/tests/order_test.rs"]
mod order_tests;
