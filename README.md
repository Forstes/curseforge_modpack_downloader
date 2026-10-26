# CurseForge modpack downloader

Installs an unpacked CurseForge Minecraft modpack into a `downloaded/` subfolder. It reads `manifest.json`, downloads the listed files into `downloaded/mods/`, and copies the contents of the manifest's `overrides/` directory into `downloaded/` (for example, `downloaded/config/` and `downloaded/defaultconfigs/`).

A [CurseForge API key](https://console.curseforge.com/) is required for the file metadata API and the CurseForge CDN. Set it in the process environment; the app does not read `.env`. Quote the key when exporting it because shell characters such as `$` otherwise change its value. Run:

```sh
export CURSEFORGE_API_KEY='your-key'
cargo run --release -- /home/rodion/Downloads/RAD3-1.20.1-0.25/
```

Downloads can be resumed by running the same command again. Existing files with the expected size and SHA-1 (when supplied by CurseForge) are reused. Files without a download URL, including files that CurseForge restricts to manual download, are reported as failures. The program exits with a nonzero status when any required file fails; other files continue downloading. Each file is held in memory for verification before writing it.
