// SPDX-License-Identifier: GPL-3.0-or-later
//! OxiGuitar in the browser, for Playground (patches/guitar-pro.yaml in github.com/daijro/openplayground): the
//! desktop window on the page's canvas, its files in Playground's Files, its plugins linked in, and the browser's audio.
//!
//! `main` runs as on the desktop, with [`start`] first: the plugins the page links in (the Guitar Pro readers and
//! writers, the SoundFont player and the Web Audio output) are registered where the host looks for plugin files, and
//! settings.json comes back from the browser's storage. Files live in memory (oxiguitar-playground's `std::fs`): a score
//! opened from Files comes in at its path (`browser:/Tabs/Song.gp5` is `/Tabs/Song.gp5` here); File > Open…, Save As…
//! and Export PDF… are Playground's dialogs; whatever OxiGuitar writes outside its own folders (a saved score, a PDF)
//! goes back to Files once the event is done ([`mirror`]), and what was asked for as a download is downloaded. "Open in
//! OxiGuitar" and dropped files open as a file dropped on the desktop window does. The sound set is fetched the first
//! time Play is pressed ([`sound_set_first`]).

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::time::Duration;

use oxiguitar_host::Level;
use oxiguitar_playground::std::fs;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

use crate::document::FULLEST;
use crate::worker::Request;
use crate::{APP, Shared, Then, message};

/// OxiGuitar's own folders in memory, never copied to Files: its settings, the plugins' folders, the sound set.
const OWN: [&str; 3] = ["/home", "/tmp", "/usr"];
/// What was saved or exported as a download (Save As > Download to device).
const DOWNLOADS: &str = "/tmp/downloads";
/// Where files opened from the computer and dropped files are kept (they go to Files when saved).
const OPENED: &str = "/OxiGuitar/Opened";
const DROPPED: &str = "/OxiGuitar/Dropped";
/// settings.json, kept in the browser's storage under [`SETTINGS_KEY`].
const SETTINGS: &str = "/home/web/.config/oxiguitar/settings.json";
const SETTINGS_KEY: &str = "oxiguitar-settings";
/// The sound set: GeneralUser GS 2.0.3 by S. Christian Collins (its licence: free to use, modify and repackage), as
/// SpessaSynth packs it (SoundFont 3, 8.4 MB), at a fixed commit. It goes in the user's soundfonts folder, where the
/// SoundFont plugin finds it.
const SOUND_SET: &str =
    "https://cdn.jsdelivr.net/gh/spessasus/SpessaSynth@6f7505087eba09bdbf345c97f5cf573fc547412e/soundfonts/GeneralUserGS.sf3";
const SOUND_SET_AT: &str = "/home/web/.local/share/oxiguitar/soundfonts/GeneralUser GS 2.0.3.sf3";

/// Where the sound set is.
#[derive(Clone, Copy, PartialEq)]
enum Sound {
    Missing,
    Fetching,
    /// Here; the player's thread is getting it ready.
    Preparing,
    Ready,
}

thread_local! {
    static SOUND: Cell<Sound> = const { Cell::new(Sound::Missing) };
    /// The Player action that waits for the sound set.
    static PLAY_WHEN_READY: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The page's side, before anything else in `main`.
pub(crate) fn start() {
    std::panic::set_hook(Box::new(|info| web_sys::console::error_1(&format!("OxiGuitar: {info}").into())));
    macro_rules! link {
        ($($name:literal => $plugin:ident),* $(,)?) => {
            $(oxiguitar_playground::libloading::register($name, $plugin::MANIFEST_JSON, $plugin::oxiguitar_plugin_v1 as *const ());)*
        };
    }
    link!(
        "gp3" => oxiguitar_gp3_plugin,
        "gp4" => oxiguitar_gp4_plugin,
        "gp5" => oxiguitar_gp5_plugin,
        "gpx" => oxiguitar_gpx_plugin,
        "gp" => oxiguitar_gp_plugin,
        "soundfont" => oxiguitar_midi_plugin,
        "audio-web" => oxiguitar_audio_web_plugin,
    );
    if let Some(text) = storage().and_then(|s| s.get_item(SETTINGS_KEY).ok().flatten()) {
        fs::put(Path::new(SETTINGS), text.into_bytes());
    }
    fs::on_change(mirror);
}

/// What the host found: the folders it made are made afresh in memory on every visit, so not worth a word.
pub(crate) fn started(mut startup: oxiguitar_host::Startup) -> oxiguitar_host::Startup {
    startup.notices.retain(|n| !n.text.starts_with("Created "));
    startup
}

/// Once the window is up: files from Playground ("Open in OxiGuitar", `?open=`) and files dropped on the window.
pub(crate) fn ready() {
    call("onOpen", &[Closure::<dyn Fn(JsValue)>::new(|file: JsValue| opened(&file, OPENED)).into_js_value()]);
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    if let Some(loading) = document.get_element_by_id("loading") {
        loading.remove();
    }
    let over = Closure::<dyn Fn(web_sys::DragEvent)>::new(|e: web_sys::DragEvent| e.prevent_default());
    let drop = Closure::<dyn Fn(web_sys::DragEvent)>::new(|e: web_sys::DragEvent| {
        e.prevent_default();
        let Some(file) = e.data_transfer().and_then(|d| d.files()).and_then(|f| f.get(0)) else {
            return;
        };
        wasm_bindgen_futures::spawn_local(async move {
            if let Ok(buffer) = wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                let path = Path::new(DROPPED).join(file.name());
                fs::put(&path, js_sys::Uint8Array::new(&buffer).to_vec());
                open(path);
            }
        });
    });
    // On the window's canvas only: a file dropped on Playground's Files dialog is the dialog's.
    if let Some(canvas) = document.get_element_by_id("canvas") {
        let _ = canvas.add_event_listener_with_callback("dragover", over.into_js_value().unchecked_ref());
        let _ = canvas.add_event_listener_with_callback("drop", drop.into_js_value().unchecked_ref());
    }
}

fn shared() -> Option<Shared> {
    APP.with(|a| a.borrow().clone())
}

/// Runs `f` with the app in Slint's event loop. What the page's callbacks and tasks do happens outside it, where a
/// Slint timer or a change to the window can wait unseen until the next input.
fn in_loop(f: impl FnOnce(&Shared) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(app) = shared() {
            f(&app);
        }
    });
}

/// Opens `path` as a file dropped on the window: unsaved changes are asked about first.
fn open(path: PathBuf) {
    in_loop(move |app| crate::unless_unsaved(app, Then::Open(path)));
}

fn get(o: &JsValue, k: &str) -> JsValue {
    js_sys::Reflect::get(o, &k.into()).unwrap_or(JsValue::UNDEFINED)
}

/// `playgroundFiles[method](...args)`; None when the page has none or it threw.
fn call(method: &str, args: &[JsValue]) -> Option<JsValue> {
    let api = Some(get(&js_sys::global(), "playgroundFiles")).filter(JsValue::is_object)?;
    let f = get(&api, method).dyn_into::<js_sys::Function>().ok()?;
    f.apply(&api, &args.iter().collect::<js_sys::Array>()).ok()
}

async fn wait(v: Option<JsValue>) -> Option<JsValue> {
    let promise = v?.dyn_into::<js_sys::Promise>().ok()?;
    wasm_bindgen_futures::JsFuture::from(promise).await.ok().filter(|v| !v.is_null() && !v.is_undefined())
}

fn object(fields: &[(&str, JsValue)]) -> JsValue {
    let o = js_sys::Object::new();
    for (k, v) in fields {
        let _ = js_sys::Reflect::set(&o, &(*k).into(), v);
    }
    o.into()
}

fn strings<'a>(list: impl IntoIterator<Item = &'a String>) -> JsValue {
    list.into_iter().map(|s| JsValue::from(s.as_str())).collect::<js_sys::Array>().into()
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// A file from Playground (`{ path, name, bytes }`) into memory at its path; one from the computer goes to `elsewhere`.
fn bring_in(file: &JsValue, elsewhere: &str) -> Option<PathBuf> {
    let name = get(file, "name").as_string()?;
    let path = match get(file, "path").as_string().and_then(|p| p.strip_prefix("browser:").map(PathBuf::from)) {
        Some(path) => path,
        None => Path::new(elsewhere).join(Path::new(&name).file_name()?),
    };
    fs::put(&path, js_sys::Uint8Array::new(&get(file, "bytes")).to_vec());
    Some(path)
}

fn opened(file: &JsValue, elsewhere: &str) {
    if let Some(path) = bring_in(file, elsewhere) {
        open(path);
    }
}

/// Where Playground's Save As put a file named `name`: a place in Files, or [`DOWNLOADS`] for a download. None when it
/// was cancelled.
async fn saved_as(name: &str, types: JsValue) -> Option<PathBuf> {
    let answer = wait(call("saveAs", &[object(&[("name", name.into()), ("types", types)])])).await?;
    if get(&answer, "download").is_truthy() {
        return Some(Path::new(DOWNLOADS).join(name));
    }
    get(&answer, "path").as_string()?.strip_prefix("browser:").map(PathBuf::from)
}

/// File → Open…: Playground's Open dialog, for the formats the plugins read.
pub(crate) fn open_dialog(app: &Shared) {
    let types = strings(&app.borrow().open_extensions);
    let asked = call("open", &[object(&[("types", types)])]);
    wasm_bindgen_futures::spawn_local(async move {
        if let Some(file) = wait(asked).await.and_then(|p| js_sys::Array::from(&p).iter().next())
            && let Some(path) = bring_in(&file, OPENED)
        {
            in_loop(move |app| app.borrow().worker.send(Request::Open(path)));
        }
    });
}

/// File → Save As…: Playground's Save As, in the format the name's extension says (the fullest when it says none), as
/// the desktop's dialog does.
pub(crate) fn save_as_dialog(app: &Shared) {
    let (stem, mut formats) = {
        let s = app.borrow();
        let stem = s.doc_name.as_deref().map_or_else(
            || "score".to_owned(),
            |n| Path::new(n).file_stem().map_or_else(|| n.to_owned(), |x| x.to_string_lossy().into_owned()),
        );
        (stem, s.save_formats.clone())
    };
    if formats.is_empty() {
        app.borrow_mut().after_save = None;
        message(app, Level::Error, "No installed plugin writes files, so nothing can be saved.");
        return;
    }
    formats.sort_by_key(|f| f.id != FULLEST);
    let first = formats.first().and_then(|f| f.extensions.first()).cloned().unwrap_or_else(|| "gp".into());
    let name = format!("{stem}.{first}");
    let types = strings(formats.iter().flat_map(|f| &f.extensions));
    wasm_bindgen_futures::spawn_local(async move {
        let Some(mut path) = saved_as(&name, types).await else {
            // Nothing waits on a save that won't happen.
            in_loop(|app| app.borrow_mut().after_save = None);
            return;
        };
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
        let format = ext
            .as_deref()
            .and_then(|e| formats.iter().find(|f| f.extensions.iter().any(|x| x.eq_ignore_ascii_case(e))))
            .unwrap_or(&formats[0]);
        if !format.extensions.iter().any(|x| ext.as_deref().is_some_and(|e| x.eq_ignore_ascii_case(e)))
            && let Some(e) = format.extensions.first()
        {
            path.set_extension(e);
        }
        let id = format.id.clone();
        in_loop(move |app| app.borrow().worker.send(Request::Save(Some((path, id)))));
    });
}

/// File → Export PDF…: Playground's Save As for the PDF.
pub(crate) fn export_dialog(app: &Shared) {
    let (name, settings) = {
        let s = app.borrow();
        (s.pdf_name.clone(), s.settings.clone())
    };
    wasm_bindgen_futures::spawn_local(async move {
        let types = strings(&["pdf".to_owned()]);
        let Some(mut path) = saved_as(&name, types).await else { return };
        if path.extension().is_none() {
            path.set_extension("pdf");
        }
        in_loop(move |app| app.borrow().worker.send(Request::ExportPdf(path, settings)));
    });
}

/// A score saved as a download has no place in Files: its next Save asks where, as Playground's dialog promises.
pub(crate) fn downloaded(app: &Shared) -> bool {
    app.borrow().folder.as_deref() == Some(Path::new(DOWNLOADS))
}

/// The score's unsaved state, for Playground's leave warning.
pub(crate) fn unsaved(modified: bool) {
    call("setUnsaved", &[modified.into()]);
}

/// Copies what OxiGuitar wrote to where it belongs: settings.json to the browser's storage, a download to the
/// computer, anything else outside its own folders to Files (or out of it, when removed).
fn mirror() {
    for p in fs::changes() {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // A file being saved, before it takes the real one's place.
        if name.starts_with('.') {
            continue;
        }
        if p == Path::new(SETTINGS) {
            if let (Some(s), Ok(text)) = (storage(), fs::read_to_string(&p)) {
                let _ = s.set_item(SETTINGS_KEY, &text);
            }
            continue;
        }
        if p.starts_with(DOWNLOADS) {
            if let Ok(bytes) = fs::read(&p) {
                call("download", &[name.into(), js_sys::Uint8Array::from(bytes.as_slice()).into()]);
            }
            continue;
        }
        if OWN.iter().any(|own| p.starts_with(own)) {
            continue;
        }
        let target = JsValue::from(format!("browser:{}", p.display()));
        match fs::metadata(&p) {
            Ok(m) if m.is_file() => {
                call("write", &[target, js_sys::Uint8Array::from(fs::read(&p).unwrap_or_default().as_slice()).into()]);
            }
            Ok(_) => {}
            Err(_) => {
                call("remove", &[target]);
            }
        }
    }
}

/// Opens a link in a new tab (OxiGuitar's links are its help pages and its donation page).
pub(crate) fn open_link(url: &str) -> Result<(), String> {
    if !oxiguitar_host::is_https(url) {
        return Err(format!("{url} is not a web address"));
    }
    web_sys::window()
        .and_then(|w| w.open_with_url_and_target(url, "_blank").ok())
        .map(|_| ())
        .ok_or_else(|| format!("The link couldn't be opened. The address is {url}"))
}

/// Tools → Plugins: the page's plugins are linked into it; none can be added, removed or set up in a window of its own.
pub(crate) fn no_plugin_windows(app: &Shared) {
    message(
        app,
        Level::Warning,
        "In the browser, OxiGuitar's plugins are built in: the Guitar Pro formats, the SoundFont player (GeneralUser GS) and Web Audio. Plugins can't be added or set up here.",
    );
}

/// Whether Play still has to fetch the sound set (or wait for it to be ready).
pub(crate) fn sound_set_missing() -> bool {
    SOUND.get() != Sound::Ready
}

/// Play or Play Selection before the sound set is here: it is fetched now (once), and the action runs once the player
/// has it. True when the action was taken here.
pub(crate) fn sound_set_first(action: &str) -> bool {
    if !matches!(action, "play" | "play-selection") || SOUND.get() == Sound::Ready {
        return false;
    }
    PLAY_WHEN_READY.set(Some(action.to_owned()));
    if SOUND.get() != Sound::Missing {
        return true;
    }
    SOUND.set(Sound::Fetching);
    status(Some("Downloading the sounds: GeneralUser GS, 8 MB…"));
    wasm_bindgen_futures::spawn_local(async {
        match fetch(SOUND_SET).await {
            Ok(bytes) => {
                fs::put(Path::new(SOUND_SET_AT), bytes);
                SOUND.set(Sound::Preparing);
                status(Some("Getting the sounds ready…"));
                // Once that is drawn: the player's thread decodes the set (a few seconds) on this one.
                in_loop(|app| {
                    let app = app.clone();
                    slint::Timer::single_shot(Duration::from_millis(100), move || crate::playing::score_changed(&app, false));
                });
            }
            Err(e) => {
                SOUND.set(Sound::Missing);
                PLAY_WHEN_READY.take();
                status(None);
                let text = format!("The sounds couldn't be downloaded ({e}). Press Play to try again.");
                in_loop(move |app| message(app, Level::Error, text));
            }
        }
    });
    true
}

/// What the sound set is doing, over the window's bottom right corner (OxiGuitar's footer holds only warnings and
/// errors); `None` takes it away.
fn status(text: Option<&str>) {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let shown = document.get_element_by_id("oxiguitar-status");
    let Some(text) = text else {
        if let Some(el) = shown {
            el.remove();
        }
        return;
    };
    let el = shown.or_else(|| {
        let el = document.create_element("div").ok()?;
        el.set_id("oxiguitar-status");
        let _ = el.set_attribute(
            "style",
            "position:fixed;right:16px;bottom:40px;z-index:10;padding:6px 12px;border-radius:6px;background:#2d6a45;\
             color:#fff;font:13px system-ui,sans-serif;pointer-events:none",
        );
        document.body()?.append_child(&el).ok()?;
        Some(el)
    });
    if let Some(el) = el {
        el.set_text_content(Some(text));
    }
}

/// The player has its instruments: the sound set is ready, and what waited for it plays.
pub(crate) fn sound_ready(app: &Shared) {
    if SOUND.get() != Sound::Preparing {
        return;
    }
    SOUND.set(Sound::Ready);
    status(None);
    if let Some(action) = PLAY_WHEN_READY.take() {
        let app = app.clone();
        slint::Timer::single_shot(Duration::ZERO, move || crate::playing::action(&app, &action));
    }
}

/// Whether the player's `text` is to be left unsaid: that no sound set is found, before Play fetched it. Once it was
/// fetched, the player says what it says.
pub(crate) fn quiet(text: &str) -> bool {
    match SOUND.get() {
        Sound::Missing | Sound::Fetching => text.contains("sound set"),
        Sound::Preparing => {
            SOUND.set(Sound::Ready);
            PLAY_WHEN_READY.take();
            status(None);
            false
        }
        Sound::Ready => false,
    }
}

async fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let window = web_sys::window().ok_or("no window")?;
    let response = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(url)).await.map_err(|e| format!("{e:?}"))?;
    let response: web_sys::Response = response.dyn_into().map_err(|_| "no response")?;
    if !response.ok() {
        return Err(format!("HTTP {}", response.status()));
    }
    let buffer = response.array_buffer().map_err(|e| format!("{e:?}"))?;
    let buffer = wasm_bindgen_futures::JsFuture::from(buffer).await.map_err(|e| format!("{e:?}"))?;
    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}

/// The desktop's file dialogs, which the web build never opens (File → Open…, Save As… and Export PDF… use
/// Playground's instead): main.rs compiles against these.
pub(crate) mod rfd {
    use std::path::{Path, PathBuf};

    pub(crate) struct FileDialog;

    impl FileDialog {
        pub(crate) fn new() -> Self {
            FileDialog
        }
        pub(crate) fn set_title(self, _title: impl Into<String>) -> Self {
            self
        }
        pub(crate) fn set_file_name(self, _name: impl Into<String>) -> Self {
            self
        }
        pub(crate) fn add_filter(self, _name: impl Into<String>, _extensions: &[impl ToString]) -> Self {
            self
        }
        pub(crate) fn set_directory(self, _path: impl AsRef<Path>) -> Self {
            self
        }
        pub(crate) fn pick_file(self) -> Option<PathBuf> {
            None
        }
        pub(crate) fn save_file(self) -> Option<PathBuf> {
            None
        }
    }
}
