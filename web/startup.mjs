// SPDX-License-Identifier: GPL-3.0-or-later

const files = window.frameElement?.oxiGuitarFiles;
if (files) {
  window.playgroundFiles = {
    ...files,
    open: async (...args) => files.open(...args),
    saveAs: async (...args) => files.saveAs(...args),
    persist: async (...args) => files.persist(...args),
  };
}

const contexts = new Set();
if (window.AudioContext) {
  window.AudioContext = class extends window.AudioContext {
    constructor(...options) {
      super(...options);
      contexts.add(this);
    }
  };
}

function resumeAudio() {
  for (const audio of contexts) {
    if (audio.state === 'suspended') audio.resume().catch(() => {});
  }
}

for (const type of ['pointerdown', 'keydown', 'touchend']) {
  addEventListener(type, resumeAudio, {capture: true});
}
addEventListener('pagehide', () => {
  for (const audio of contexts) {
    if (audio.state !== 'closed') audio.close().catch(() => {});
  }
  contexts.clear();
});

try {
  const bindings = await import('./oxiguitar.js');
  window.wasmBindings = bindings;
  try {
    const wasm = await bindings.default();
    dispatchEvent(new CustomEvent('TrunkApplicationStarted', {detail: {wasm}}));
  } catch (error) {
    // Winit hands control to the browser after the Slint window starts.
    if (document.getElementById('loading') ||
        error.message !== "Using exceptions for control flow, don't mind me. This isn't actually an error!") throw error;
  }
} catch (error) {
  const message = 'OxiGuitar could not start. ' + (error?.message || String(error));
  const loading = document.getElementById('loading');
  if (loading) loading.textContent = message;
  files?.failed(message);
}
