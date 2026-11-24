// SPDX-License-Identifier: GPL-3.0-or-later

const revision = /^[a-f0-9]{64}$/;
const invalidName = /[/\\:\u0000-\u001f]/;
const scoreTypes = ['.gp', '.gpx', '.gp5', '.gp4', '.gp3'];
const maxBytes = 64 * 1024 * 1024;

function element(tag, properties) {
  return Object.assign(document.createElement(tag), properties);
}

function basename(path) {
  return path.split('/').at(-1);
}

function workspacePath(path) {
  if (typeof path !== 'string') throw Error('Choose a file in My files.');
  const relative = path.replace(/^(?:browser:)?\//, '');
  if (relative.split('/').some(part => !part || part === '.' || part === '..' || invalidName.test(part))) {
    throw Error('Choose a file in My files.');
  }
  return relative;
}

export function mount(host) {
  const owner = host.storage.workspaceOwner();
  const session = host.session();
  const controller = new AbortController();
  const {signal} = controller;
  const root = element('section', {className: 'window-content oxiguitar-workspace'});
  const view = element('iframe', {title: 'OxiGuitar', allow: 'autoplay'});
  const status = element('p', {textContent: 'Starting OxiGuitar…'});
  status.setAttribute('role', 'status');
  root.append(element('link', {rel: 'stylesheet', href: new URL('./workspace.css', import.meta.url).href}), view, status);

  const opened = new Map();
  let context, pending, openFile, unregister, binding;
  let dirty = false;
  let transferring = false;
  let startResolve, startReject;
  const start = new Promise((resolve, reject) => {
    startResolve = resolve;
    startReject = reject;
  });
  const timer = setTimeout(() => startReject(Error('OxiGuitar took too long to start.')), 90000);
  signal.addEventListener('abort', () => startReject(signal.reason), {once: true});
  start.catch(() => {});

  function check() {
    if (signal.aborted || host.session() !== session || host.storage.workspaceOwner() !== owner || !context?.valid()) {
      throw new DOMException('This Files session ended.', 'AbortError');
    }
    if (transferring) throw Error('Files are being transferred.');
  }

  function run(operation) {
    if (pending) return Promise.reject(Error('OxiGuitar is waiting for Files.'));
    pending = Promise.resolve().then(() => {
      check();
      return operation();
    });
    return pending.catch(error => {
      if (!signal.aborted) status.textContent = error.message;
      throw error;
    }).finally(() => { pending = undefined; });
  }

  async function read(path) {
    check();
    const loaded = await context.files.read(path, {signal});
    check();
    if (!revision.test(loaded.sha256)) throw Error('Files returned an invalid revision.');
    if (loaded.data.byteLength > maxBytes) throw Error('OxiGuitar can open scores up to 64 MiB.');
    opened.set(path, loaded.sha256);
    return {name: basename(path), path: 'browser:/' + path, bytes: loaded.data, sha256: loaded.sha256};
  }

  function bind(path, sha256, action) {
    binding = path ? {path, sha256} : undefined;
    host.desktop.updateAccountFileBinding(host.w, {owner, path, sha256});
    status.textContent = action && path ? action + ' ' + basename(path) : 'OxiGuitar is ready';
  }

  view.oxiGuitarFiles = {
    workspace: true,
    onOpen(callback) { openFile = callback; },
    ready() { startResolve(); },
    failed(message) { startReject(Error(message)); },
    setUnsaved(value) { dirty = value; },
    open() {
      return run(async () => {
        const choice = await host.storage.pickWorkspaceFile({extensions: scoreTypes, signal});
        return choice ? [await read(workspacePath(choice))] : null;
      });
    },
    opened(path) {
      check();
      const source = path == null ? null : workspacePath(path);
      const sha256 = opened.get(source);
      opened.delete(source);
      if (sha256) bind(source, sha256, 'Opened');
      else bind(null, null);
    },
    saveAs({name, types}) {
      return run(async () => {
        const allowed = types.map(type => '.' + type.toLowerCase()).filter(ext => ['.gp', '.gp5', '.pdf'].includes(ext));
        if (!allowed.length) throw Error('This file format is unavailable.');
        const folder = await host.storage.pickWorkspaceFile({foldersOnly: true, signal});
        if (!folder) return null;
        const choice = await host.ui.prompt('File name', {title: 'Save OxiGuitar file', value: name, confirmLabel: 'Save', signal});
        if (choice == null) return null;
        let filename = choice.trim();
        if (!filename || invalidName.test(filename) || filename === '.' || filename === '..') throw Error('Enter a file name.');
        if (!allowed.some(ext => filename.toLowerCase().endsWith(ext))) filename += allowed[0];
        const path = workspacePath((folder === '/' ? '' : folder + '/') + filename);
        return {path: 'browser:/' + path};
      });
    },
    persist(path, bytes, isPdf) {
      return run(async () => {
        const target = workspacePath(path);
        if (bytes.byteLength > maxBytes) throw Error('OxiGuitar can save files up to 64 MiB.');
        const expected = !isPdf && target === binding?.path ? binding.sha256 : null;
        let saved;
        try {
          saved = await context.files.write(target, new Blob([bytes], {type: isPdf ? 'application/pdf' : 'application/octet-stream'}), expected, {signal});
        } catch (error) {
          if (error?.status === 409) throw Error('This file already exists or changed. Save as a new file to keep both copies.');
          throw error;
        }
        check();
        if (!revision.test(saved.sha256)) throw Error('Files did not confirm the save.');
        if (isPdf) status.textContent = 'Exported ' + basename(target);
        else bind(target, saved.sha256, 'Saved');
        return true;
      });
    },
  };

  const ready = (async () => {
    context = await host.storage.workspaceFileContext();
    check();
    if (context.owner !== owner) throw Error('This editor belongs to another Files workspace.');
    view.src = new URL('./native/index.html', import.meta.url).href;
    await start;
    check();
    unregister = context.registerFileWriter?.(async () => {
      if (pending) await pending;
      if (dirty) throw Error('Save the OxiGuitar score before transferring Files.');
      transferring = true;
      view.inert = true;
      return () => {
        transferring = false;
        view.inert = false;
      };
    });
    status.textContent = 'OxiGuitar is ready';
    const launch = host.w.launchContext;
    if (launch) {
      if (launch.storage !== 'account' || launch.owner !== owner) throw Error('Open the score from My files.');
      const score = await read(workspacePath(launch.path));
      if (!launch.restore && launch.sha256 && launch.sha256 !== score.sha256) throw Error('The score changed in Files. Open its current revision.');
      openFile(score);
    }
  })().finally(() => clearTimeout(timer));
  ready.catch(error => { if (!signal.aborted) status.textContent = error.message; });

  return {
    element: root,
    ready,
    focus: () => view.focus(),
    inspect: () => ({path: binding?.path ?? null, dirty}),
    async requestClose() {
      if (pending) await pending.catch(() => {});
      return !dirty || host.ui.confirm('Discard unsaved changes to this score?', {confirmLabel: 'Discard', signal});
    },
    dispose() {
      controller.abort();
      unregister?.();
    },
  };
}
