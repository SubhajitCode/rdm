# rdm Chrome Extension

A Manifest V3 Chrome extension that intercepts browser downloads and detected streaming media,
routing them to the `rdm` Rust Download Manager backend running locally on `127.0.0.1:8597`.

---

## How to load it in Chrome (Developer / Unpacked mode)

> No build step is required. The extension uses plain ES modules.

### Step 1 — Enable Developer mode

1. Open Chrome and go to `chrome://extensions`
2. Toggle **Developer mode** on (top-right corner of the page)

### Step 2 — Load the extension

1. Click **"Load unpacked"** (top-left area, visible after enabling Developer mode)
2. Navigate to and select the **`rdm-extension/`** folder inside this repository
   (the folder that contains `manifest.json`)
3. Click **"Select Folder"** (or **"Open"** on macOS)

The extension now appears in the list as **"rdm Integration"** with a blue icon in
the Chrome toolbar.

### Step 3 — Start rdm

The extension communicates with the rdm native server over HTTP.  
Start it before using the extension:

```bash
# From the repo root
cargo run --bin rdm -- serve
# or, if already built:
./target/release/rdm serve
```

The server must be listening on `http://127.0.0.1:8597`.

Once rdm is running the toolbar icon turns **blue** and the popup shows the active state.

---

## Using the extension

### Popup

Click the **rdm** icon in the toolbar to open the popup.

| State | What you see |
|---|---|
| rdm not running | Warning message + "Launch rdm" button |
| rdm running but monitoring off | "Browser monitoring is disabled" message |
| rdm running, monitoring on | Video list + monitoring toggle |

- **Monitoring toggle** — turn browser-level interception on/off without stopping rdm.
- **Video items** — click any detected video to send it to rdm for download.
- **Clear** — removes all detected videos from the list.

### Download interception

When monitoring is enabled, any file whose extension matches rdm's configured list
(e.g. `.zip`, `.mp4`, `.mkv`) is automatically cancelled in Chrome and handed off
to rdm instead.

### Context menu

Right-click any **link, image, video, or audio element** on a page:

- **Download with rdm** — send the link target to rdm
- **Download Image with rdm** — send the image source to rdm

### Media detection badge

When rdm detects streaming media (e.g. a video playing on a website), the badge
counter on the toolbar icon increments. Open the popup to download any captured item.

---

## Updating the extension after code changes

1. Go to `chrome://extensions`
2. Find **rdm Integration** and click the **refresh icon** (circular arrow)

Chrome reloads the service worker and popup from disk — no need to re-add the extension.

---

## Troubleshooting

| Symptom | Fix |
|---|---|
| Icon stays grey / popup shows "rdm is not running" | Start `rdm serve` |
| Popup shows "Browser monitoring is disabled" | Enable monitoring in rdm's settings |
| Downloads not intercepted | Check that the file extension is in rdm's `fileExts` list |
| Service worker errors in DevTools | Go to `chrome://extensions` → click "Service Worker" link under rdm Integration |

To view extension logs: open the **Service Worker** DevTools console from
`chrome://extensions` and filter by `[rdm]`.

---

## File structure

```
rdm-extension/
├── manifest.json               MV3 manifest
├── src/
│   ├── background/
│   │   ├── main.js             Entry point — creates App and calls start()
│   │   ├── app.js              Central orchestrator (download/media/tabs/popup)
│   │   ├── connector.js        HTTP IPC + alarm-based keep-alive polling
│   │   ├── request-watcher.js  webRequest media interceptor
│   │   └── logger.js           Console wrapper with [rdm] prefix
│   └── popup/
│       ├── popup.html/js/css   Normal state (monitoring on)
│       ├── error.html/js       rdm not running
│       └── disabled.html       Monitoring off in rdm settings
├── icons/
│   ├── icon{16,48,128}.png     Active (blue)
│   └── icon{16,48,128}-mono.png Inactive (grey)
└── _locales/en_US/messages.json i18n strings
```
