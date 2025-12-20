/// Filesystem abstractions. Used to represent directory trees, such as the input project or
/// lowered Rust source.
///
/// # Freezing
///
/// These types are read-only. To create them:
/// 1. Create the file/directory/symlink in the diagnostic directory and populate it as intended.
/// 2. "freeze" the file using `Reporter::freeze_path` or (TODO: what ToolReporter function or
///    related thing do tools use?).
///
/// Freezing the contents will:
/// 1. Make the on-disk structures read-only. This applies recursively, but does not follow
///    symlinks.
/// 2. Construct a `DirEntry` representing the on-disk structure.
///
/// After you have frozen a filesystem object, it (and everything else frozen with it, if it is a
/// directory) must be left unchanged in the diagnostic directory. This is to avoid the need to
/// store the contents of files in memory.
///
/// # Symlinks
///
/// Symlink resolution is always performed in the context of a particular `Dir`, and acts as if
/// that particular `Dir` is floating in space (i.e., it does not know the `Dir`'s parent or its
/// location relative to the filesystem root). As a result, absolute symlinks cannot be followed.
/// Further, it means that whether or not a symlink is resolvable (and what it resolves to) can
/// depend on which `Dir` you use to query a symlink.
///
/// For example, suppose you create the following directory structure then freeze it (and call the
/// frozen directory `a`):
///
/// ```shell
/// $ mkdir c
/// $ ln -s '../b' c/d
/// $ ls -l . c/
/// .:
/// total 4
/// -rw-rw-r-- 1 ryan ryan    0 Dec 17 16:05 b
/// drwxrwxr-x 2 ryan ryan 4096 Dec 17 16:05 c
///
/// c/:
/// total 0
/// lrwxrwxrwx 1 ryan ryan 4 Dec 17 16:05 d -> ../b
/// ```
///
/// If you resolve `b/d` from the context of `a/`, then it will resolve to `c`. But if you instead
/// retrieve the `b/` `Dir` and try to resolve `d` from them, resolution will fail (because the
/// resolution traverses outside `b/`).
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::iter::once;
use std::os::unix::fs::symlink;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

/// View of a read-only directory element.
#[derive(Clone)]
pub enum DirEntry {
    Dir(Dir),
    File(File),
    Symlink(Symlink),
}

/// A DirEntry after symlinks have been fully resolved.
#[derive(Clone)]
pub enum ResolvedEntry {
    Dir(Dir),
    File(File),
}

// Note: File and TextFile are internally Arc<> to a single shared type. That way, the UTF-8-ness
// of the file can be shared between the copies, because it is computed lazily.
/// A read-only file.
#[derive(Clone)]
pub struct File {}
/// A read-only UTF-8 file.
#[derive(Clone)]
pub struct TextFile {}

/// A frozen directory.
#[derive(Clone)]
pub struct Dir {
    contents: Arc<HashMap<OsString, DirEntry>>,
}

impl Dir {
    /// Iterates through the contents of this directory.
    pub fn entries(&self) -> impl Iterator<Item = (OsString, DirEntry)> {
        self.contents.iter().map(|(p, e)| (p.clone(), e.clone()))
    }

    /// Retrieves the entry at the specified location under this directory. This will resolve
    /// symlinks, but only if they are relative and do not traverse outside this `Dir`.
    // TODO: Unit test. Edge cases:
    // 1. Symlink that points back to the original Dir (i.e. './a -> .').
    // 2. Exponential symlinks, i.e.:
    //    a
    //    b -> .
    //    c -> b/b/b/b/b/b/b/b/b/b
    //    d -> c/c/c/c/c/c/c/c/c/c
    //    ...
    // 3. Circular symlinks, including circular variants of the previous one.
    // 4. Absolute symlinks
    // 5. Symlinks that traverse outside the Dir.
    // 6. Embed one Dir into two different Dirs, evaluate a symlink that uses .. to point to
    //    different locations in each parent dir.
    // 7. Paths that try to traverse through a file.
    pub fn get<P: AsRef<Path>>(&self, path: P) -> Result<ResolvedEntry, GetError> {
        self.get_inner(path.as_ref())
    }

    /// Retrieves the entry at the specified location. If you want a recursive lookup (traversing
    /// into subdirectories), use [get] instead.
    /// Returns `None` if there is no entry at `name`.
    pub fn get_entry<N: AsRef<OsStr>>(&self, name: N) -> Option<DirEntry> {
        self.contents.get(name.as_ref()).cloned()
    }

    /// The implementation of [get]. The only difference is this is not generic.
    fn get_inner(&self, path: &Path) -> Result<ResolvedEntry, GetError> {
        // Suppose you want to resolve the entry at this path:
        //   name1/name2/name3/name4.txt
        // If symlinks do not exist, then you can work left to right, calling `get_entry()` at each
        // step to find the file. There are a couple ways this can fail:
        //   1. File not found: one of the names does not exist.
        //   2. Not a directory: a path component that is not the last is a file (i.e. name3), so
        //      you cannot descend into it.
        //
        // Now, lets add a bit of complexity:
        //   name1/name2/../name3/./name4.txt
        // Handling `.` is easy, all you have to do is to ignore it. But `..` requires a bit more
        // work. You can handle it by keeping a vector of `&Dir`s tracing back upwards to the root.
        // Then, when you encounter `..`, you just pop the last &Dir from the vector to go up a
        // level. This does add one new error condition:
        //   3. Leaves directory: the path traverses outside this Dir (e.g. name1/../../name2.txt)
        //
        // Now, lets add symlinks. The naive way is to continue left to right, and when you
        // encounter symlinks, substitute the symlink's name for its path. For example, if
        // name1/name2 is a symlink that points to ../name3, then the path:
        //   name1/name2/name4.txt.
        // would be resolved by performing the following substitution (upon detecting that name2 is
        // a symlink):
        //   name1/../name3/name4.txt.
        //         ^^^^^^^^ Substituted symlink contents.
        // However, there are two issues with this approach:
        //   1. Symlink loops: resolving a symlink can require resolving that same symlink forever.
        //      This algorithm would run out of memory or loop infinitely.
        //   2. Exponential lookup time. If you have the following symlinks in a directory:
        //      a -> .
        //      b -> a/a/a/a/a/a/a/a/a/a
        //      c -> b/b/b/b/b/b/b/b/b/b
        //      d -> c/c/c/c/c/c/c/c/c/c
        //      Then resolving d/ requires 1000 steps.
        //
        // To solve both issues, we add a symlink cache. The cache is a hashmap with the following
        // configuration:
        //   Key: The direct path to the symlink. By "direct", I mean that none of the path
        //        components other than the symlinks themselves are symlinks.
        //   Value: `None` if we are currently trying to resolve that symlink, and the canonical
        //          path the symlink resolves to if we have already resolved the symlink.
        // When we first encounter a new symlink, we create a cache entry for it (which is
        // initially None), and add a new "save symlink" value into the path. For example, suppose
        // the input path is:
        //   name1/name2/name4.txt
        // and name1/name2 is a symlink that points to ../name3. Then upon encountering name2 a new
        // cache entry is created:
        //   name1/name2 -> None
        // and the path is updated to read:
        //   name1/../name3/SaveSymlink("name1/name2")/name4.txt
        //         ^^^^^^^^ ^^^^^^^^^^^^^^^^^^^^^^^^^^
        //    Symlink value   The "save symlink" value
        // When the algorithm reaches the SaveSymlink("name1/name2") entry, the path looks like:
        //   name3/SaveSymlink("name1/name2")/name4.txt
        // At this point, the cache entry is updated to:
        //   name1/name2 -> name3
        // Of course, if we encounter a symlink that is in the cache, we just substitute the
        // symlink's target for the entire portion of the path left-of-and-including the symlink
        // itself.
        // If we encounter a symlink that is in the cache but which has a target of None, then that
        // means that symlink is a loop and cannot be resolved. In that case, we can immediately
        // return an error.

        // Path to the current directory. This is the portion of the path from the algorithm
        // description that is left of the next component the algorithm will look at. That is, if
        // the current directory is *self, then `current` is empty.
        // We store both the name and the Dir for each element. The Dir is stored to make
        // evaluating `..` efficient, and the name is stored to make cache lookups
        // possible/efficient.
        #[derive(Clone)]
        struct ParentDir<'d> {
            dir: &'d Dir,
            name: &'d OsStr,
        }
        let mut current: Vec<ParentDir> = Vec::new();

        // The portion of the path that we have not resolved yet, including `Save` entries. This is
        // stored from right-to-left, as we only ever want to modify the leftmost portion of the
        // remaining path.
        enum Step<'p> {
            Component(Component<'p>), // A component from the original path.
            Save(PathBuf),            // Instruction to update the symlink cache.
        }
        let mut remaining: Vec<_> = path.components().rev().map(Step::Component).collect();

        // The symlink cache. The key and value are as described in the comment at the top of this
        // file. However, the path is stored as a vec of ParentDir to make it more efficient to
        // update `current`.
        let mut cache = HashMap::new();

        // Main loop: consume the leftmost component of `remaining` at each iteration. When an
        // iteration completes, `current` will be updated to represent the impact of that
        // component.
        while let Some(step) = remaining.pop() {
            // Handle Save steps first.
            let component = match step {
                Step::Component(component) => component,
                Step::Save(symlink_path) => {
                    debug_assert!(cache.insert(symlink_path, Some(current.clone())).is_some());
                    continue;
                }
            };
            // Handle all the non-Normal components.
            let name = match component {
                // An absolute path automatically points outside a Dir.
                Component::Prefix(_) | Component::RootDir => return Err(GetError::LeavesDir),
                Component::CurDir => continue,
                Component::ParentDir => match current.pop() {
                    None => return Err(GetError::LeavesDir),
                    Some(_) => continue,
                },
                Component::Normal(name) => name,
            };
            // Look up this new path component's DirEntry, and handle files and directories.
            let cur_dir = current.last().map(|p| p.dir).unwrap_or(self);
            let symlink = match cur_dir.contents.get(name).ok_or(GetError::NotFound)? {
                DirEntry::Dir(dir) => {
                    current.push(ParentDir { dir, name });
                    continue;
                }
                // If we encounter a file, there are only two possibilities: remaining is empty and
                // we are done, or this is a NotADirectory error.
                DirEntry::File(file) => {
                    return match remaining.iter().any(|s| matches!(s, Step::Component(_))) {
                        false => Ok(ResolvedEntry::File(file.clone())),
                        true => Err(GetError::NotADirectory),
                    };
                }
                DirEntry::Symlink(symlink) => symlink,
            };
            let link_path: PathBuf = current.iter().map(|p| p.name).chain(once(name)).collect();
        }

        todo!()
    }
}

#[derive(Debug, Error)]
pub enum GetError {
    #[error("path leaves the Dir")]
    LeavesDir,
    #[error("intermediate path component is a file")]
    NotADirectory,
    #[error("file or directory not found")]
    NotFound,
}

/// A symlink that has been frozen. Note that the thing it points to is not frozen; in fact it may
/// not exist or may be entirely outside the diagnostics directory.
#[derive(Clone)]
pub struct Symlink {
    // The path contained by this symlink.
    contents: Arc<Path>,
}

impl Symlink {
    pub fn contents(&self) -> &Path {
        &self.contents
    }

    /// Writes this symlink into the filesystem at the given path.
    pub fn write_rw<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        symlink(&self.contents, path)
    }
}
