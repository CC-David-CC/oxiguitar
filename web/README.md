# OxiGuitar in WebOS

The browser build uses OxiGuitar v0.11.0 at demo revision `cb2fa058022ebdaadb604c852746a9a1187becbd` and the GPL browser port from OpenPlayground revision `4317d6b7ec991926713fd1af1bc715c42c0e8a55`. It runs the Slint score editor with the linked Guitar Pro and SoundFont plugins. It reads `.gp3`, `.gp4`, `.gp5`, `.gpx` and `.gp`, saves editable `.gp5` and `.gp`, and exports PDF. Use File → Open and File → Save to edit scores in My files. Use Save As to preserve another revision when the original changes.

Build from the repository root using Rust 1.99.1 with the `wasm32-unknown-unknown` target and Trunk 0.21.14.

```sh
trunk build --release --locked
cd target/trunk-dist
curl --fail --location https://raw.githubusercontent.com/spessasus/SpessaSynth/6f7505087eba09bdbf345c97f5cf573fc547412e/soundfonts/GeneralUserGS.sf3 -o GeneralUserGS.sf3
printf '%s  %s\n' e2ed326ff44d15f78f2fdc72403b6fa6b77ee7266d3aad0d2198bc95797bc66c GeneralUserGS.sf3 | sha256sum --check
```

Package the Trunk output under `native/`, `web/mount.mjs` as `index.mjs` and `web/workspace.css` beside it. Playback loads the bundled GeneralUser GS 2.0.3 SoundFont by S. Christian Collins on the first Play. Ship the [SoundFont license](https://github.com/mrbumpy409/GeneralUser-GS/blob/684543d5e5efaef08d02be50dcda8d552478fa60/documentation/LICENSE.txt), the app's GPL license, the applicable library notices, and access to the corresponding source for the published WASM build.

The installed browser plugins handle Guitar Pro files and SoundFont playback. Desktop plugins and track plugin settings sidecars are outside this build.
