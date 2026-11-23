// SPDX-License-Identifier: MIT OR Apache-2.0
//! Plugins linked into one program, for Playground's browser build (patches/guitar-pro.yaml in
//! github.com/daijro/openplayground): a page can't load `.oxp` files, so the plugins it uses are linked into it, each
//! sharing this crate. The glue's one plugin per library ([`super::PLUGIN`]) is one per plugin type here: the table of
//! plugin `P` calls `start` and `call` through [`start`] and [`call`], which say whose call it is.

use std::any::TypeId;
use std::cell::{Cell, RefCell};

use oxiguitar_plugin_api::abi::{HostTableV1, RawBytes, RawSink, Status};

use crate::Plugin;

thread_local! {
    /// The plugin whose `start` or `call` runs (a plugin's call can reach another's through the host).
    static CURRENT: Cell<Option<TypeId>> = const { Cell::new(None) };
    /// Every plugin started, by type; never dropped, as a library is never unloaded.
    static STARTED: RefCell<Vec<(TypeId, &'static Box<dyn Plugin>)>> = const { RefCell::new(Vec::new()) };
}

/// The plugin of the call running, as the glue's `PLUGIN` holds it in a plugin of its own.
pub(super) struct Plugins;

impl Plugins {
    pub(super) fn get(&self) -> Option<&'static Box<dyn Plugin>> {
        let current = CURRENT.get()?;
        STARTED.with_borrow(|s| s.iter().find(|(t, _)| *t == current).map(|(_, p)| *p))
    }

    pub(super) fn set(&self, plugin: Box<dyn Plugin>) -> Result<(), Box<dyn Plugin>> {
        let Some(current) = CURRENT.get() else {
            return Err(plugin);
        };
        if self.get().is_some() {
            return Err(plugin);
        }
        STARTED.with_borrow_mut(|s| s.push((current, Box::leak(Box::new(plugin)))));
        Ok(())
    }
}

/// Runs `f` as plugin `P`'s call.
fn as_plugin<P: Plugin, T>(f: impl FnOnce() -> T) -> T {
    let before = CURRENT.replace(Some(TypeId::of::<P>()));
    let out = f();
    CURRENT.set(before);
    out
}

/// `PluginTableV1::start` for plugin `P`.
///
/// # Safety
///
/// As [`super::start`].
pub(super) unsafe extern "C" fn start<P: Plugin>(host: *const HostTableV1, answer: *mut RawSink) -> Status {
    // SAFETY: the caller's guarantees, passed on.
    as_plugin::<P, _>(|| unsafe { super::start::<P>(host, answer) })
}

/// `PluginTableV1::call` for plugin `P`.
///
/// # Safety
///
/// As [`super::call`].
pub(super) unsafe extern "C" fn call<P: Plugin>(request: RawBytes, answer: *mut RawSink) -> Status {
    // SAFETY: the caller's guarantees, passed on.
    as_plugin::<P, _>(|| unsafe { super::call(request, answer) })
}
