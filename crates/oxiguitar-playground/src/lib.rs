// SPDX-License-Identifier: GPL-3.0-or-later
//! OxiGuitar in the browser, for Playground (patches/guitar-pro.yaml in github.com/daijro/openplayground).
//!
//! wasm32-unknown-unknown has no clock, no files, no threads and no dlopen: `Instant::now` panics there, every file call
//! fails, `thread::spawn` panics and a channel's `recv` can never wait. The modules that use them take `std` from here
//! instead, through one `#[cfg(target_arch = "wasm32")] use oxiguitar_playground::std;` line at each one's end (a local
//! `std` shadows the standard library's name in that module; everything else in it still comes from the real std):
//!   time     web-time's Instant and SystemTime;
//!   fs       an in-memory file system (scores, settings.json, the sound set), each change noted for the page to mirror
//!            into Playground's Files;
//!   thread   `spawn` runs what it is given once the current event is done, on this thread; `task` runs a future (the
//!            worker's, the player's and the plugin tabs' loops, made async on the web by the patches); `scope` runs
//!            each part at once;
//!   mpsc     a channel whose `recv` is awaited;
//!   env      `HOME` is `/home/web`, the program is `/usr/bin/oxiguitar`, `temp_dir` is `/tmp`;
//!   process  `id` is 1.
//! And `libloading`: the plugins linked into the page, registered by name, in place of the `.oxp` files the desktop
//! loads (the host's loader takes it on the web through the same kind of `use` line).
#![cfg(target_arch = "wasm32")]

pub mod std {
    pub use ::std::*;

    pub mod time {
        pub use web_time::*;
    }

    pub mod process {
        pub use ::std::process::*;

        /// The page is one process (std's panics on wasm).
        pub fn id() -> u32 {
            1
        }
    }

    pub mod env {
        pub use ::std::env::*;
        use ::std::ffi::{OsStr, OsString};
        use ::std::path::PathBuf;

        /// `HOME` is `/home/web` (settings live in memory, kept in the browser's storage by the page). Nothing else is
        /// set.
        pub fn var_os<K: AsRef<OsStr>>(key: K) -> Option<OsString> {
            (key.as_ref() == "HOME").then(|| OsString::from("/home/web"))
        }

        pub fn var<K: AsRef<OsStr>>(key: K) -> Result<String, VarError> {
            var_os(key).and_then(|v| v.into_string().ok()).ok_or(VarError::NotPresent)
        }

        /// Where the program is, as on Linux: its plugins are in /usr/lib/oxiguitar/plugins.
        pub fn current_exe() -> ::std::io::Result<PathBuf> {
            Ok(PathBuf::from("/usr/bin/oxiguitar"))
        }

        /// A folder in the in-memory files (std's panics on wasm).
        pub fn temp_dir() -> PathBuf {
            PathBuf::from("/tmp")
        }
    }

    pub mod thread {
        pub use ::std::thread::*;
        use ::std::cell::RefCell;
        use ::std::future::Future;
        use ::std::num::NonZeroUsize;
        use ::std::rc::Rc;

        /// What [`spawn`] gives back: its result, once it has run.
        pub struct JoinHandle<T>(Rc<RefCell<Option<T>>>);

        // The page has one thread.
        unsafe impl<T> Send for JoinHandle<T> {}
        unsafe impl<T> Sync for JoinHandle<T> {}

        impl<T> JoinHandle<T> {
            /// Its result if it has run; nothing waits on the page.
            pub fn join(self) -> Result<T> {
                self.0.borrow_mut().take().ok_or_else(|| Box::new("it hasn't run yet") as _)
            }

            pub fn is_finished(&self) -> bool {
                self.0.borrow().is_some()
            }
        }

        /// Runs `f` once the current event is done, on this thread.
        pub fn spawn<F, T>(f: F) -> JoinHandle<T>
        where
            F: FnOnce() -> T + Send + 'static,
            T: Send + 'static,
        {
            let out = Rc::new(RefCell::new(None));
            let filled = out.clone();
            wasm_bindgen_futures::spawn_local(async move {
                *filled.borrow_mut() = Some(f());
            });
            JoinHandle(out)
        }

        /// Runs `f` as a task of the page (the loops that wait on a channel, made async on the web).
        pub fn task(f: impl Future<Output = ()> + 'static) -> JoinHandle<()> {
            let out = Rc::new(RefCell::new(None));
            let filled = out.clone();
            wasm_bindgen_futures::spawn_local(async move {
                f.await;
                *filled.borrow_mut() = Some(());
            });
            JoinHandle(out)
        }

        /// Lets the page draw and take input before going on.
        pub async fn yield_now() {
            let mut yielded = false;
            ::std::future::poll_fn(|cx| {
                if yielded {
                    return ::std::task::Poll::Ready(());
                }
                yielded = true;
                cx.waker().wake_by_ref();
                ::std::task::Poll::Pending
            })
            .await
        }

        pub struct Builder;

        impl Builder {
            pub fn new() -> Builder {
                Builder
            }
            pub fn name(self, _name: String) -> Builder {
                self
            }
            pub fn stack_size(self, _size: usize) -> Builder {
                self
            }
            pub fn spawn<F, T>(self, f: F) -> ::std::io::Result<JoinHandle<T>>
            where
                F: FnOnce() -> T + Send + 'static,
                T: Send + 'static,
            {
                Ok(spawn(f))
            }
        }

        pub fn available_parallelism() -> ::std::io::Result<NonZeroUsize> {
            Ok(NonZeroUsize::MIN)
        }

        /// A scope whose threads run at once, one after another.
        pub struct Scope;

        pub struct ScopedJoinHandle<T>(T);

        impl<T> ScopedJoinHandle<T> {
            pub fn join(self) -> Result<T> {
                Ok(self.0)
            }
        }

        impl Scope {
            pub fn spawn<F: FnOnce() -> T, T>(&self, f: F) -> ScopedJoinHandle<T> {
                ScopedJoinHandle(f())
            }
        }

        pub fn scope<F: FnOnce(&Scope) -> T, T>(f: F) -> T {
            f(&Scope)
        }
    }

    pub mod sync {
        pub use ::std::sync::*;

        /// A channel on the page's one thread: `recv` is awaited (the loops that wait on one are async on the web).
        pub mod mpsc {
            pub use ::std::sync::mpsc::{RecvError, RecvTimeoutError, SendError, TryRecvError};
            use ::std::cell::{Cell, RefCell};
            use ::std::collections::VecDeque;
            use ::std::future::Future;
            use ::std::rc::Rc;
            use ::std::task::{Poll, Waker};

            struct Shared<T> {
                queue: RefCell<VecDeque<T>>,
                waker: RefCell<Option<Waker>>,
                senders: Cell<usize>,
            }

            impl<T> Shared<T> {
                fn wake(&self) {
                    if let Some(w) = self.waker.borrow_mut().take() {
                        w.wake();
                    }
                }
            }

            pub struct Sender<T>(Rc<Shared<T>>);
            pub struct Receiver<T>(Rc<Shared<T>>);

            // The page has one thread.
            unsafe impl<T> Send for Sender<T> {}
            unsafe impl<T> Sync for Sender<T> {}
            unsafe impl<T> Send for Receiver<T> {}

            pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
                let shared = Rc::new(Shared { queue: RefCell::default(), waker: RefCell::default(), senders: Cell::new(1) });
                (Sender(shared.clone()), Receiver(shared))
            }

            impl<T> Sender<T> {
                pub fn send(&self, t: T) -> Result<(), SendError<T>> {
                    if Rc::strong_count(&self.0) == self.0.senders.get() {
                        return Err(SendError(t));
                    }
                    self.0.queue.borrow_mut().push_back(t);
                    self.0.wake();
                    Ok(())
                }
            }

            impl<T> Clone for Sender<T> {
                fn clone(&self) -> Self {
                    self.0.senders.set(self.0.senders.get() + 1);
                    Sender(self.0.clone())
                }
            }

            impl<T> Drop for Sender<T> {
                fn drop(&mut self) {
                    self.0.senders.set(self.0.senders.get() - 1);
                    self.0.wake();
                }
            }

            impl<T> std::fmt::Debug for Sender<T> {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("Sender")
                }
            }

            impl<T> Receiver<T> {
                /// The next message, once there is one; an error once every sender is gone.
                pub fn recv(&self) -> impl Future<Output = Result<T, RecvError>> + '_ {
                    ::std::future::poll_fn(move |cx| {
                        if let Some(t) = self.0.queue.borrow_mut().pop_front() {
                            return Poll::Ready(Ok(t));
                        }
                        if self.0.senders.get() == 0 {
                            return Poll::Ready(Err(RecvError));
                        }
                        *self.0.waker.borrow_mut() = Some(cx.waker().clone());
                        Poll::Pending
                    })
                }

                pub fn try_recv(&self) -> Result<T, TryRecvError> {
                    match self.0.queue.borrow_mut().pop_front() {
                        Some(t) => Ok(t),
                        None if self.0.senders.get() == 0 => Err(TryRecvError::Disconnected),
                        None => Err(TryRecvError::Empty),
                    }
                }

                pub fn try_iter(&self) -> impl Iterator<Item = T> + '_ {
                    ::std::iter::from_fn(move || self.try_recv().ok())
                }
            }
        }
    }

    pub mod fs {
        pub use ::std::fs::*;
        use ::std::cell::RefCell;
        use ::std::collections::{BTreeMap, BTreeSet};
        use ::std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
        use ::std::path::{Component, Path, PathBuf};

        enum Entry {
            Dir,
            File(Vec<u8>, web_time::SystemTime),
        }

        #[derive(Default)]
        struct Tree {
            entries: BTreeMap<PathBuf, Entry>,
            /// Paths written, created or removed since the last [`changes`].
            changed: BTreeSet<PathBuf>,
            /// Told when a change is noted (the page then mirrors it once the event is done).
            notify: Option<fn()>,
        }

        thread_local! {
            static TREE: RefCell<Tree> = RefCell::default();
        }

        fn with<T>(f: impl FnOnce(&mut Tree) -> T) -> T {
            TREE.with(|t| f(&mut t.borrow_mut()))
        }

        /// `/a/./b/../c` -> `/a/c`: one spelling per file (relative paths start at `/`).
        pub fn key(p: &Path) -> PathBuf {
            let mut out = PathBuf::from("/");
            for c in p.components() {
                match c {
                    Component::ParentDir => {
                        out.pop();
                    }
                    Component::Normal(n) => out.push(n),
                    _ => {}
                }
            }
            out
        }

        fn missing(p: &Path) -> io::Error {
            io::Error::new(io::ErrorKind::NotFound, format!("{} does not exist", p.display()))
        }

        impl Tree {
            fn mark(&mut self, p: PathBuf) {
                self.changed.insert(p);
                if let Some(f) = self.notify {
                    // After this borrow: the page's callback only schedules its work.
                    wasm_bindgen_futures::spawn_local(async move { f() });
                }
            }
            fn parents(&mut self, p: &Path) {
                for dir in p.ancestors().skip(1) {
                    self.entries.entry(dir.to_path_buf()).or_insert(Entry::Dir);
                }
            }
            fn put(&mut self, p: PathBuf, bytes: Vec<u8>) {
                self.parents(&p);
                self.entries.insert(p, Entry::File(bytes, web_time::SystemTime::now()));
            }
            /// `p` and everything below it.
            fn below(&self, p: &Path) -> Vec<PathBuf> {
                self.entries.range(p.to_path_buf()..).map(|(k, _)| k).take_while(|k| k.starts_with(p)).cloned().collect()
            }
        }

        /// Stores a file without noting a change (files the page brings in from Files or the network).
        pub fn put(p: &Path, bytes: Vec<u8>) {
            with(|t| t.put(key(p), bytes));
        }

        /// The paths changed since the last call: each now holds what the page's storage should hold, or is gone.
        pub fn changes() -> Vec<PathBuf> {
            with(|t| ::std::mem::take(&mut t.changed).into_iter().collect())
        }

        /// `f` is called (once the current event is done) after each change.
        pub fn on_change(f: fn()) {
            with(|t| t.notify = Some(f));
        }

        pub fn exists<P: AsRef<Path>>(p: P) -> io::Result<bool> {
            Ok(metadata(p).is_ok())
        }

        pub fn write<P: AsRef<Path>, C: AsRef<[u8]>>(p: P, bytes: C) -> io::Result<()> {
            let p = key(p.as_ref());
            with(|t| {
                t.put(p.clone(), bytes.as_ref().to_vec());
                t.mark(p);
            });
            Ok(())
        }

        pub fn read<P: AsRef<Path>>(p: P) -> io::Result<Vec<u8>> {
            TREE.with(|t| match t.borrow().entries.get(&key(p.as_ref())) {
                Some(Entry::File(bytes, _)) => Ok(bytes.clone()),
                _ => Err(missing(p.as_ref())),
            })
        }

        pub fn read_to_string<P: AsRef<Path>>(p: P) -> io::Result<String> {
            String::from_utf8(read(p)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }

        pub fn create_dir_all<P: AsRef<Path>>(p: P) -> io::Result<()> {
            let p = key(p.as_ref());
            with(|t| {
                if !t.entries.contains_key(&p) {
                    t.parents(&p);
                    t.entries.insert(p, Entry::Dir);
                }
            });
            Ok(())
        }

        pub fn create_dir<P: AsRef<Path>>(p: P) -> io::Result<()> {
            if metadata(p.as_ref()).is_ok() {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} exists", p.as_ref().display())));
            }
            create_dir_all(p)
        }

        pub fn remove_file<P: AsRef<Path>>(p: P) -> io::Result<()> {
            let p = key(p.as_ref());
            with(|t| match t.entries.get(&p) {
                Some(Entry::File(..)) => {
                    t.entries.remove(&p);
                    t.mark(p);
                    Ok(())
                }
                _ => Err(missing(&p)),
            })
        }

        pub fn remove_dir_all<P: AsRef<Path>>(p: P) -> io::Result<()> {
            let p = key(p.as_ref());
            with(|t| {
                let gone = t.below(&p);
                if gone.is_empty() {
                    return Err(missing(&p));
                }
                for g in gone {
                    t.entries.remove(&g);
                    t.mark(g);
                }
                Ok(())
            })
        }

        pub fn rename<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> io::Result<()> {
            let (from, to) = (key(from.as_ref()), key(to.as_ref()));
            let bytes = read(&from)?;
            with(|t| {
                t.entries.remove(&from);
                t.mark(from);
                t.put(to.clone(), bytes);
                t.mark(to);
            });
            Ok(())
        }

        pub fn copy<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> io::Result<u64> {
            let bytes = read(from)?;
            let n = bytes.len() as u64;
            write(to, bytes)?;
            Ok(n)
        }

        pub fn canonicalize<P: AsRef<Path>>(p: P) -> io::Result<PathBuf> {
            metadata(p.as_ref())?;
            Ok(key(p.as_ref()))
        }

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct FileType(bool);
        impl FileType {
            pub fn is_file(&self) -> bool {
                self.0
            }
            pub fn is_dir(&self) -> bool {
                !self.0
            }
            pub fn is_symlink(&self) -> bool {
                false
            }
        }

        #[derive(Clone, Debug)]
        pub struct Metadata(Option<(u64, web_time::SystemTime)>);
        impl Metadata {
            pub fn len(&self) -> u64 {
                self.0.map_or(0, |(n, _)| n)
            }
            pub fn is_file(&self) -> bool {
                self.0.is_some()
            }
            pub fn is_dir(&self) -> bool {
                self.0.is_none()
            }
            pub fn is_symlink(&self) -> bool {
                false
            }
            pub fn file_type(&self) -> FileType {
                FileType(self.is_file())
            }
            pub fn modified(&self) -> io::Result<web_time::SystemTime> {
                Ok(self.0.map_or(web_time::SystemTime::UNIX_EPOCH, |(_, t)| t))
            }
        }

        pub fn metadata<P: AsRef<Path>>(p: P) -> io::Result<Metadata> {
            TREE.with(|t| match t.borrow().entries.get(&key(p.as_ref())) {
                Some(Entry::File(bytes, at)) => Ok(Metadata(Some((bytes.len() as u64, *at)))),
                Some(Entry::Dir) => Ok(Metadata(None)),
                None => Err(missing(p.as_ref())),
            })
        }

        pub fn symlink_metadata<P: AsRef<Path>>(p: P) -> io::Result<Metadata> {
            metadata(p)
        }

        pub struct DirEntry(PathBuf);
        impl DirEntry {
            pub fn path(&self) -> PathBuf {
                self.0.clone()
            }
            pub fn file_name(&self) -> ::std::ffi::OsString {
                self.0.file_name().unwrap_or_default().to_os_string()
            }
            pub fn metadata(&self) -> io::Result<Metadata> {
                metadata(&self.0)
            }
            pub fn file_type(&self) -> io::Result<FileType> {
                Ok(metadata(&self.0)?.file_type())
            }
        }

        pub type ReadDir = ::std::vec::IntoIter<io::Result<DirEntry>>;

        pub fn read_dir<P: AsRef<Path>>(p: P) -> io::Result<ReadDir> {
            let dir = key(p.as_ref());
            if !metadata(&dir)?.is_dir() {
                return Err(io::Error::other(format!("{} is not a folder", dir.display())));
            }
            let entries = TREE.with(|t| {
                t.borrow().below(&dir).into_iter().filter(|k| k.parent() == Some(&dir)).map(|k| Ok(DirEntry(k))).collect::<Vec<_>>()
            });
            Ok(entries.into_iter())
        }

        /// A file's bytes, read whole when it opens; one opened for writing is stored back when flushed or dropped.
        pub struct File {
            data: Cursor<Vec<u8>>,
            /// Where written bytes go, for a file opened to write.
            to: Option<PathBuf>,
            dirty: bool,
        }

        impl File {
            pub fn open<P: AsRef<Path>>(p: P) -> io::Result<File> {
                Ok(File { data: Cursor::new(read(p)?), to: None, dirty: false })
            }
            pub fn create<P: AsRef<Path>>(p: P) -> io::Result<File> {
                write(p.as_ref(), [])?;
                Ok(File { data: Cursor::new(Vec::new()), to: Some(key(p.as_ref())), dirty: false })
            }
            pub fn metadata(&self) -> io::Result<Metadata> {
                Ok(Metadata(Some((self.data.get_ref().len() as u64, web_time::SystemTime::now()))))
            }
            pub fn sync_all(&self) -> io::Result<()> {
                Ok(())
            }
            fn store(&mut self) {
                if let (true, Some(to)) = (self.dirty, &self.to) {
                    let _ = write(to, self.data.get_ref());
                    self.dirty = false;
                }
            }
        }

        impl Read for File {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.data.read(buf)
            }
        }

        impl Seek for File {
            fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
                self.data.seek(pos)
            }
        }

        impl Write for File {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                if self.to.is_none() {
                    return Err(io::Error::new(io::ErrorKind::PermissionDenied, "the file is open to read"));
                }
                self.dirty = true;
                self.data.write(buf)
            }
            fn flush(&mut self) -> io::Result<()> {
                self.store();
                Ok(())
            }
        }

        impl Drop for File {
            fn drop(&mut self) {
                self.store();
            }
        }

        /// std's options, for the few the program uses: create_new, create, write, append.
        #[derive(Clone, Debug, Default)]
        pub struct OpenOptions {
            write: bool,
            append: bool,
            create: bool,
            create_new: bool,
            truncate: bool,
        }

        impl OpenOptions {
            pub fn new() -> OpenOptions {
                OpenOptions::default()
            }
            pub fn read(&mut self, _on: bool) -> &mut OpenOptions {
                self
            }
            pub fn write(&mut self, on: bool) -> &mut OpenOptions {
                self.write = on;
                self
            }
            pub fn append(&mut self, on: bool) -> &mut OpenOptions {
                self.append = on;
                self
            }
            pub fn create(&mut self, on: bool) -> &mut OpenOptions {
                self.create = on;
                self
            }
            pub fn create_new(&mut self, on: bool) -> &mut OpenOptions {
                self.create_new = on;
                self
            }
            pub fn truncate(&mut self, on: bool) -> &mut OpenOptions {
                self.truncate = on;
                self
            }
            pub fn open<P: AsRef<Path>>(&self, p: P) -> io::Result<File> {
                let p = p.as_ref();
                let old = read(p).ok();
                if self.create_new && old.is_some() {
                    return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} exists", p.display())));
                }
                if old.is_none() && !(self.create || self.create_new) {
                    return Err(missing(p));
                }
                let writes = self.write || self.append;
                let made = old.is_none() || (writes && self.truncate);
                let data = if made { Vec::new() } else { old.unwrap_or_default() };
                let mut cursor = Cursor::new(data);
                if self.append {
                    cursor.seek(SeekFrom::End(0))?;
                }
                if made {
                    write(p, [])?;
                }
                Ok(File { data: cursor, to: writes.then(|| key(p)), dirty: false })
            }
        }
    }
}

/// The plugins linked into the page, in place of plugin files (`.oxp` libraries the desktop loads with libloading):
/// each registered with its manifest and entry function at `/usr/lib/oxiguitar/plugins/<name>.oxp`, the folder the
/// host looks in beside `/usr/bin/oxiguitar`. The host finds them with [`files`], reads their manifests with
/// [`manifest`] and "loads" them through [`Library`].
pub mod libloading {
    use std::cell::RefCell;
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    /// The system plugin folder of a program at /usr/bin/oxiguitar.
    pub const FOLDER: &str = "/usr/lib/oxiguitar/plugins";

    struct Linked {
        path: PathBuf,
        manifest: &'static [u8],
        entry: *const (),
    }

    thread_local! {
        static LINKED: RefCell<Vec<Linked>> = const { RefCell::new(Vec::new()) };
    }

    /// Registers the plugin `name` (its manifest.json and its entry function, `oxiguitar_plugin_v1`, as a pointer).
    pub fn register(name: &str, manifest: &'static str, entry: *const ()) {
        let path = Path::new(FOLDER).join(format!("{name}.oxp"));
        LINKED.with_borrow_mut(|l| l.push(Linked { path, manifest: manifest.as_bytes(), entry }));
    }

    /// The plugins "in" `folder`, by name.
    pub fn files(folder: &Path) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> =
            LINKED.with_borrow(|l| l.iter().filter(|p| p.path.parent() == Some(folder)).map(|p| p.path.clone()).collect());
        found.sort();
        found
    }

    /// The manifest of the plugin at `path`.
    pub fn manifest(path: &Path) -> Option<Vec<u8>> {
        LINKED.with_borrow(|l| l.iter().find(|p| p.path == path).map(|p| p.manifest.to_vec()))
    }

    #[derive(Debug)]
    pub struct Error(String);

    impl std::fmt::Display for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }

    impl std::error::Error for Error {}

    /// A plugin linked into the page.
    #[derive(Debug)]
    pub struct Library(*const ());

    /// A symbol of it: its entry function.
    pub struct Symbol<T>(T);

    impl<T> std::ops::Deref for Symbol<T> {
        type Target = T;
        fn deref(&self) -> &T {
            &self.0
        }
    }

    impl Library {
        /// # Safety
        /// As libloading's: the plugin registered at `path` is trusted like the program (it is part of it).
        pub unsafe fn new<P: AsRef<OsStr>>(path: P) -> Result<Library, Error> {
            let path = Path::new(path.as_ref());
            LINKED
                .with_borrow(|l| l.iter().find(|p| p.path == path).map(|p| Library(p.entry)))
                .ok_or_else(|| Error(format!("{} isn't one of this page's plugins", path.display())))
        }

        /// The entry function, whatever `symbol` is asked for (each plugin has that one).
        ///
        /// # Safety
        /// `T` is the entry function's type, a function pointer.
        pub unsafe fn get<T>(&self, _symbol: &[u8]) -> Result<Symbol<T>, Error> {
            assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*const ()>());
            Ok(Symbol(unsafe { std::mem::transmute_copy::<*const (), T>(&self.0) }))
        }
    }
}
