//! Virtual filesystem, or "guest filesystem".
//!
//! This lets us put files and directories where the guest app expects them to
//! be, without constraining the layout of the host filesystem.
//!
//! Currently the filesystem layout is frozen at the point of creation. Except
//! for creating new files in existing directories, no nodes can be created,
//! deleted, renamed or moved.
//!
//! All files in the guest filesystem have a corresponding file in the host
//! filesystem. Accessing a file requires traversing the guest filesystem's
//! directory structure to find out the host path, but after that is done, the
//! host file is accessed directly; there is no virtualization of file I/O.
//!
//! Directories only need a corresponding directory in the host filesystem if
//! they are writeable (i.e. if new files can be created in them).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug)]
enum FsNode {
    File {
        host_path: PathBuf,
        writeable: bool,
    },
    Directory {
        children: HashMap<String, FsNode>,
        writeable: Option<PathBuf>,
    },
}
impl FsNode {
    fn from_host_dir(host_path: &Path, writeable: bool) -> Self {
        let mut children = HashMap::new();
        for entry in std::fs::read_dir(host_path).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            let host_path = entry.path();
            let name = entry.file_name().into_string().unwrap();

            if kind.is_symlink() {
                unimplemented!("Symlink: {:?}", host_path);
            } else if kind.is_file() {
                children.insert(
                    name,
                    FsNode::File {
                        host_path,
                        writeable,
                    },
                );
            } else if kind.is_dir() {
                children.insert(name, FsNode::from_host_dir(&host_path, writeable));
            } else {
                panic!("{:?} is not a symlink, file or directory", host_path);
            }
        }
        FsNode::Directory {
            children,
            writeable: match writeable {
                true => Some(host_path.to_owned()),
                false => None,
            },
        }
    }

    // Convenience methods for constructing the read-only parts of the initial
    // filesystem layout

    fn dir() -> Self {
        FsNode::Directory {
            children: HashMap::new(),
            writeable: None,
        }
    }
    fn with_child(mut self, name: &str, child: FsNode) -> Self {
        let FsNode::Directory { ref mut children, writeable: _ } = self else {
            panic!();
        };
        assert!(children.insert(String::from(name), child).is_none());
        self
    }
    fn file(host_path: PathBuf) -> Self {
        FsNode::File {
            host_path,
            writeable: false,
        }
    }
}

/// Like [Path] but for the virtual filesystem.
#[repr(transparent)]
#[derive(Debug)]
pub struct GuestPath(str);
impl GuestPath {
    pub fn new<S: AsRef<str>>(s: &S) -> &GuestPath {
        unsafe { &*(s.as_ref() as *const str as *const GuestPath) }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Join a path component.
    ///
    /// This should use `AsRef<GuestPath>`, but we can't have a blanket
    /// implementation of `AsRef<GuestPath>` for all `AsRef<str>` types, so we
    /// would have to implement it for everything that can derference to `&str`.
    /// It's easier to just use `&str`.
    pub fn join<P: AsRef<str>>(&self, path: P) -> GuestPathBuf {
        GuestPathBuf::from(format!("{}/{}", self.as_str(), path.as_ref()))
    }

    /// Get the final component of the path.
    pub fn file_name(&self) -> Option<&str> {
        // FIXME: this should do the same resolution as `std::path::file_name()`
        let (_, file_name) = self.as_str().rsplit_once('/')?;
        Some(file_name)
    }
}
impl AsRef<GuestPath> for GuestPath {
    fn as_ref(&self) -> &Self {
        self
    }
}
impl AsRef<str> for GuestPath {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
impl AsRef<GuestPath> for str {
    fn as_ref(&self) -> &GuestPath {
        unsafe { &*(self as *const str as *const GuestPath) }
    }
}
impl std::borrow::ToOwned for GuestPath {
    type Owned = GuestPathBuf;

    fn to_owned(&self) -> GuestPathBuf {
        GuestPathBuf::from(self)
    }
}

/// Like [PathBuf] but for the virtual filesystem.
#[derive(Debug, Clone)]
pub struct GuestPathBuf(String);
impl From<String> for GuestPathBuf {
    fn from(string: String) -> GuestPathBuf {
        GuestPathBuf(string)
    }
}
impl From<&GuestPath> for GuestPathBuf {
    fn from(guest_path: &GuestPath) -> GuestPathBuf {
        guest_path.as_str().to_string().into()
    }
}
impl From<GuestPathBuf> for String {
    fn from(guest_path: GuestPathBuf) -> String {
        guest_path.0
    }
}
impl std::ops::Deref for GuestPathBuf {
    type Target = GuestPath;

    fn deref(&self) -> &GuestPath {
        let s: &str = &self.0;
        s.as_ref()
    }
}
impl AsRef<GuestPath> for GuestPathBuf {
    fn as_ref(&self) -> &GuestPath {
        self
    }
}
impl std::borrow::Borrow<GuestPath> for GuestPathBuf {
    fn borrow(&self) -> &GuestPath {
        self
    }
}

fn apply_path_component<'a>(components: &mut Vec<&'a str>, component: &'a str) {
    match component {
        "" => (),
        "." => (),
        ".." => {
            components.pop();
        }
        _ => components.push(component),
    }
}

/// Resolve a path so that it is absolute and has no `.`, `..` or empty
/// components. The result is a series of zero or more path components forming
/// an absolute path (e.g. `["foo", "bar"]` means `/foo/bar`).
///
/// `relative_to` is the starting point for resolving a relative path, e.g. the
/// current directory. It must be an absolute path. It is optional if `path`
/// is absolute.
fn resolve_path<'a>(path: &'a GuestPath, relative_to: Option<&'a GuestPath>) -> Vec<&'a str> {
    let mut components = Vec::new();

    if !path.as_str().starts_with('/') {
        let relative_to = relative_to.unwrap().as_str();
        assert!(relative_to.starts_with('/'));
        for component in relative_to.split('/') {
            apply_path_component(&mut components, component);
        }
    }

    for component in path.as_str().split('/') {
        apply_path_component(&mut components, component);
    }

    components
}

/// Like [std::fs::OpenOptions] but for the guest filesystem.
/// TODO: `create_new`.
pub struct GuestOpenOptions {
    read: bool,
    write: bool,
    append: bool,
    create: bool,
    truncate: bool,
}
impl GuestOpenOptions {
    pub fn new() -> GuestOpenOptions {
        GuestOpenOptions {
            read: false,
            write: false,
            append: false,
            create: false,
            truncate: false,
        }
    }
    pub fn read(&mut self) -> &mut Self {
        self.read = true;
        self
    }
    pub fn write(&mut self) -> &mut Self {
        self.write = true;
        self
    }
    pub fn append(&mut self) -> &mut Self {
        self.append = true;
        self
    }
    pub fn create(&mut self) -> &mut Self {
        self.create = true;
        self
    }
    pub fn truncate(&mut self) -> &mut Self {
        self.truncate = true;
        self
    }
}

/// Handles host I/O errors by panicking. This is intended specifically for
/// opening files. The assumption is that the guest filesystem contains all the
/// information needed to tell if opening a file should succeed, so if opening
/// the file nonetheless fails, there's either a bug or the user has done
/// something wrong.
fn handle_open_err<T>(open_result: std::io::Result<T>, host_path: &Path) -> T {
    match open_result {
        Ok(ok) => ok,
        Err(e) => panic!("Unexpected I/O failure when trying to access real path {:?}: {}. This might indicate that files needed by touchHLE are missing, or were moved while it was running.", host_path, e),
    }
}

/// The type that owns the guest filesystem and provides accessors for it.
#[derive(Debug)]
pub struct Fs {
    root: FsNode,
    current_directory: GuestPathBuf,
    home_directory: GuestPathBuf,
}
impl Fs {
    /// Construct a filesystem containing a home directory for the app, its
    /// bundle and documents, and the bundled shared libraries. Returns the new
    /// filesystem and the guest path of the bundle.
    ///
    /// The `bundle_dir_name` argument will be used as the name of the bundle
    /// directory in the guest filesystem, and must end in `.app`.
    /// This allows the host directory for the bundle to be renamed from its
    /// original name without confusing the app. Supposedly Apple does something
    /// similar when executing iOS apps on modern Macs.
    ///
    /// The `bundle_id` argument should be some value that uniquely identifies
    /// the app. This will be used to construct the host path for the app's
    /// sandbox directory, where documents can be stored. A directory will be
    /// created at that path if it does not already exist.
    pub fn new(
        bundle_host_path: &Path,
        bundle_dir_name: String,
        bundle_id: &str,
    ) -> (Fs, GuestPathBuf) {
        const FAKE_UUID: &str = "00000000-0000-0000-0000-000000000000";

        let home_directory = GuestPathBuf::from(format!("/User/Applications/{}", FAKE_UUID));
        let current_directory = home_directory.clone();

        let bundle_guest_path = home_directory.join(&bundle_dir_name);

        let documents_host_path = Path::new("touchHLE_sandbox")
            .join(bundle_id)
            .join("Documents");
        if let Err(e) = std::fs::create_dir_all(&documents_host_path) {
            panic!(
                "Could not create documents directory for app at {:?}: {:?}",
                documents_host_path, e
            );
        }

        // Some Free Software libraries are bundled with touchHLE.
        let dylibs_host_path = Path::new("touchHLE_dylibs");
        let usr_lib = FsNode::dir()
            .with_child(
                "libgcc_s.1.dylib",
                FsNode::file(dylibs_host_path.join("libgcc_s.1.dylib")),
            )
            .with_child(
                // symlink
                "libstdc++.6.dylib",
                FsNode::file(dylibs_host_path.join("libstdc++.6.0.4.dylib")),
            )
            .with_child(
                "libstdc++.6.0.4.dylib",
                FsNode::file(dylibs_host_path.join("libstdc++.6.0.4.dylib")),
            );

        let root = FsNode::dir()
            .with_child(
                "User",
                FsNode::dir().with_child(
                    "Applications",
                    FsNode::dir().with_child(
                        FAKE_UUID,
                        FsNode::Directory {
                            children: HashMap::from([
                                (
                                    bundle_dir_name,
                                    FsNode::from_host_dir(
                                        bundle_host_path,
                                        /* writeable: */ false,
                                    ),
                                ),
                                (
                                    "Documents".to_string(),
                                    FsNode::from_host_dir(
                                        &documents_host_path,
                                        /* writeable: */ true,
                                    ),
                                ),
                            ]),
                            writeable: None,
                        },
                    ),
                ),
            )
            .with_child("usr", FsNode::dir().with_child("lib", usr_lib));

        log_dbg!("Initial filesystem layout: {:#?}", root);

        (
            Fs {
                root,
                current_directory,
                home_directory,
            },
            bundle_guest_path,
        )
    }

    /// Get the absolute path of the guest app's (sandboxed) home directory.
    pub fn home_directory(&self) -> &GuestPath {
        &self.home_directory
    }

    /// Get the node at a given path, if it exists.
    fn lookup_node(&self, path: &GuestPath) -> Option<&FsNode> {
        let mut node = &self.root;
        for component in resolve_path(path, Some(&self.current_directory)) {
            let FsNode::Directory { children, writeable: _ } = node else {
                return None;
            };
            node = children.get(component)?
        }
        Some(node)
    }

    /// Get the parent of the node at a given path, if it exists, and return it
    /// together with the final path component. This is an alternative to
    /// [Self::lookup_node] useful when writing to a file, where it might not
    /// exist yet (but its parent directory does).
    fn lookup_parent_node(&mut self, path: &GuestPath) -> Option<(&mut FsNode, String)> {
        let components = resolve_path(path, Some(&self.current_directory));
        let (&final_component, parent_components) = components.split_last()?;

        let mut parent = &mut self.root;
        for &component in parent_components {
            let FsNode::Directory { children, writeable: _ } = parent else {
                return None;
            };
            parent = children.get_mut(component)?
        }

        Some((parent, final_component.to_string()))
    }

    /// Like [std::path::Path::is_file] but for the guest filesystem.
    pub fn is_file(&self, path: &GuestPath) -> bool {
        matches!(self.lookup_node(path), Some(FsNode::File { .. }))
    }

    /// Like [std::fs::read] but for the guest filesystem.
    pub fn read<P: AsRef<GuestPath>>(&self, path: P) -> Result<Vec<u8>, ()> {
        let node = self.lookup_node(path.as_ref()).ok_or(())?;
        let FsNode::File {
            host_path,
            writeable: _,
        } = node else {
            return Err(())
        };
        Ok(handle_open_err(std::fs::read(host_path), host_path))
    }

    /// Like [std::fs::File::open] but for the guest filesystem.
    #[allow(dead_code)]
    pub fn open<P: AsRef<GuestPath>>(&self, path: P) -> Result<std::fs::File, ()> {
        let node = self.lookup_node(path.as_ref()).ok_or(())?;
        let FsNode::File {
            host_path,
            writeable: _,
        } = node else {
            return Err(())
        };
        Ok(handle_open_err(std::fs::File::open(host_path), host_path))
    }

    /// Like [std::fs::File::options] but for the guest filesystem.
    pub fn open_with_options<P: AsRef<GuestPath>>(
        &mut self,
        path: P,
        options: GuestOpenOptions,
    ) -> Result<std::fs::File, ()> {
        let GuestOpenOptions {
            read,
            write,
            append,
            create,
            truncate,
        } = options;
        assert!((!truncate && !create) || write || append);

        let path = path.as_ref();

        let (parent_node, new_filename) = self.lookup_parent_node(path).ok_or(())?;
        let FsNode::Directory {
            children,
            writeable: dir_host_path,
        } = parent_node else {
            return Err(());
        };

        // Open an existing file if possible

        if let Some(existing_file) = children.get(&new_filename) {
            let FsNode::File {
                host_path,
                writeable,
            } = existing_file else {
                return Err(());
            };
            if !writeable && (append || write) {
                log!("Warning: attempt to write to read-only file {:?}", path);
                return Err(());
            }
            return Ok(handle_open_err(
                std::fs::File::options()
                    .read(read)
                    .write(write)
                    .append(append)
                    .create(false)
                    .truncate(truncate)
                    .open(host_path),
                host_path,
            ));
        };

        // Create a new file otherwise

        if !create {
            return Err(());
        }

        let Some(dir_host_path) = dir_host_path else {
            log!("Warning: attempt to create file at path {:?}, but directory is read-only", path);
            return Err(());
        };

        for c in new_filename.chars() {
            if std::path::is_separator(c) {
                panic!("Attempt to create file at path {:?}, but filename contains path separator character {:?}!", path, c);
            }
        }

        let host_path = dir_host_path.join(&new_filename);

        let file = handle_open_err(
            std::fs::File::options()
                .read(read)
                .write(write)
                .append(append)
                .create(create)
                .truncate(truncate)
                .open(&host_path),
            &host_path,
        );
        log_dbg!(
            "Created file at path {:?} (host path: {:?})",
            path,
            host_path
        );
        children.insert(
            new_filename,
            FsNode::File {
                host_path,
                writeable: true,
            },
        );
        Ok(file)
    }
}
