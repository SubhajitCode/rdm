// src/background/app.js

const SUPPORTED_PROTOCOLS = ['http:', 'https:', 'ftp:'];

class App {
    constructor() {
        this.log = new Logger();

        // ── State ─────────────────────────────────────────────────────────────
        this.videoList    = [];     // streaming video items from rdm sync payload
        this.blockedHosts = [];     // hosts to skip for download takeover
        this.fileExts     = [];     // extensions that trigger download takeover
        this.tabsWatcher  = [];     // URL patterns whose tab title changes are reported
        this.userDisabled = false;  // user toggled monitoring off via popup
        this.appEnabled   = false;  // rdm server says monitoring is on
        this.activeTabId  = -1;

        // ── Collaborators ─────────────────────────────────────────────────────
        this.connector = new Connector(
            this.onMessage.bind(this),
            this.onDisconnect.bind(this)
        );
        this.requestWatcher = new RequestWatcher(
            this.onRequestDataReceived.bind(this)
        );
    }

    // ─── Lifecycle ────────────────────────────────────────────────────────────

    async start() {
        // Restore persisted user preference before connecting, so
        // isMonitoringEnabled() is correct from the first sync response.
        const stored = await browser.storage.local.get('userDisabled');
        this.userDisabled = stored.userDisabled === true;

        // Register listeners first so no requests are missed while syncing.
        this._register();
        // Await the first sync so config (mediaExts, matchingHosts, etc.) and
        // appEnabled are populated before any captured requests are forwarded.
        await this.connector.connect();
        this.log.log('App started');
    }

    // ─── Sync / connection callbacks ──────────────────────────────────────────

    onMessage(msg) {
        this.appEnabled   = msg.enabled   === true;
        this.fileExts     = msg.fileExts     ?? [];
        this.blockedHosts = msg.blockedHosts ?? [];
        this.tabsWatcher  = msg.tabsWatcher  ?? [];
        this.videoList    = msg.videoList    ?? [];

        this.requestWatcher.updateConfig({
            mediaExts:     msg.requestFileExts ?? [],
            blockedHosts:  msg.blockedHosts    ?? [],
            matchingHosts: msg.matchingHosts   ?? [],
            mediaTypes:    msg.mediaTypes      ?? [],
            urlPatterns:   msg.urlPatterns     ?? [],
        });

        this._updateActionIcon();
        this.log.log('Sync received', { appEnabled: this.appEnabled, videos: this.videoList.length });
    }

    onDisconnect(err) {
        this.log.warn('rdm server unreachable:', err.message);
        this._updateActionIcon();
    }

    // ─── webRequest media relay ───────────────────────────────────────────────

    onRequestDataReceived(data) {
        if (this.isMonitoringEnabled()) {
            this.connector.postMessage('/media', data);
        }
    }

    // ─── Download takeover ────────────────────────────────────────────────────

    _onDownloadCreated(download) {
        // Firefox does not implement onDeterminingFilename (Chrome-only).
        // Use onCreated instead: cancel + erase the browser download, then
        // hand it off to rdm.
        if (!this.isMonitoringEnabled()) return;

        const url = download.finalUrl || download.url;

        if (this._shouldTakeOver(url, download.filename)) {
            this.log.log('Intercepting download:', url);

            browser.downloads.cancel(download.id).then(() => {
                browser.downloads.erase({ id: download.id });
            });

            const referer =
                download.referrer ||
                (download.finalUrl && download.finalUrl !== download.url
                    ? download.url
                    : undefined);

            this._triggerDownload(url, download.filename, referer,
                download.fileSize, download.mime);
        }
    }

    _shouldTakeOver(url, filename) {
        if (!this._isSupportedProtocol(url)) return false;

        let hostname;
        try {
            hostname = new URL(url).hostname.toLowerCase();
        } catch {
            return false;
        }

        if (this.blockedHosts.some(h => hostname.includes(h.toLowerCase()))) return false;

        const path = (filename || new URL(url).pathname).toUpperCase();
        return this.fileExts.some(ext => path.endsWith(ext.toUpperCase()));
    }

    _triggerDownload(url, file, referer, size, mime) {
        browser.cookies.getAll({ url }).then(cookies => {
            const cookieStr = cookies?.map(c => `${c.name}=${c.value}`).join('; ');

            const requestHeaders = { 'User-Agent': [navigator.userAgent] };
            if (referer) requestHeaders['Referer'] = [referer];

            const responseHeaders = {};
            if (size > 0) responseHeaders['Content-Length'] = [String(size)];
            if (mime)     responseHeaders['Content-Type']   = [mime];

            this.connector.postMessage('/download', {
                url,
                cookie:          cookieStr,
                requestHeaders,
                responseHeaders,
                filename:        file,
                fileSize:        size,
                mimeType:        mime,
            });
        }).catch(err => this.log.error('cookies.getAll failed:', err));
    }

    // ─── Tab title tracking ───────────────────────────────────────────────────

    _onTabUpdate(tabId, changeInfo, tab) {
        if (!this.isMonitoringEnabled()) return;
        if (!changeInfo.title) return;
        if (!tab.url) return;

        const isWatched = this.tabsWatcher?.some(pattern => tab.url.includes(pattern));
        if (isWatched) {
            this.connector.postMessage('/tab-update', {
                tabUrl:   tab.url,
                tabTitle: changeInfo.title,
            });
        }
    }

    _onTabActivated(activeInfo) {
        this.activeTabId = activeInfo.tabId;
    }

    // ─── Popup message handler ────────────────────────────────────────────────

    _onPopupMessage(request, sender, sendResponse) {
        switch (request.type) {
            case 'stat':
                sendResponse({
                    enabled: this.isMonitoringEnabled(),
                    list:    this.videoList,
                });
                break;

            case 'cmd':
                this.userDisabled = request.enabled === false;
                // Persist across event-page wake/sleep cycles.
                browser.storage.local.set({ userDisabled: this.userDisabled });
                if (request.enabled && !this.connector.isConnected()) {
                    this.connector.launchApp();
                }
                this._updateActionIcon();
                sendResponse({ ok: true });
                break;

            case 'vid':
                this.connector.postMessage('/vid', { vid: String(request.itemId) });
                sendResponse({ ok: true });
                break;

            case 'clear':
                this.connector.postMessage('/clear', {});
                this.videoList = [];
                this._updateActionIcon();
                sendResponse({ ok: true });
                break;

            default:
                this.log.warn('Unknown popup message type:', request.type);
        }
    }

    // ─── Context menu ─────────────────────────────────────────────────────────

    _attachContextMenu() {
        browser.contextMenus.create({
            id:       'rdm-download-link',
            title:    'Download with rdm',
            contexts: ['link', 'video', 'audio', 'all'],
        });
        browser.contextMenus.create({
            id:       'rdm-download-image',
            title:    'Download Image with rdm',
            contexts: ['image'],
        });
    }

    _onMenuClicked(info, tab) {
        if (info.menuItemId === 'rdm-download-link') {
            const url = [info.linkUrl, info.srcUrl, info.pageUrl]
                .find(u => this._isSupportedProtocol(u));
            if (url) this._triggerDownload(url, null, info.pageUrl, null, null);
        }
        if (info.menuItemId === 'rdm-download-image') {
            const url = [info.srcUrl, info.linkUrl, info.pageUrl]
                .find(u => this._isSupportedProtocol(u));
            if (url) this._triggerDownload(url, null, info.pageUrl, null, null);
        }
    }

    // ─── Action icon / popup routing ──────────────────────────────────────────

    _updateActionIcon() {
        this._updateActionIconAsync().catch(err => this.log.warn('setIcon failed:', err));
    }

    async _updateActionIconAsync() {
        const active = this.isMonitoringEnabled();
        const suffix = active ? '' : '-mono';

        // Firefox supports setting icon path directly (no need for ImageData).
        const iconPaths = {
            16:  `icons/icon16${suffix}.png`,
            48:  `icons/icon48${suffix}.png`,
            128: `icons/icon128${suffix}.png`,
        };
        await browser.action.setIcon({ path: iconPaths });

        const count = this.videoList?.length ?? 0;
        await browser.action.setBadgeText({ text: count > 0 ? String(count) : '' });
        await browser.action.setBadgeBackgroundColor({ color: '#2196F3' });

        if (!this.connector.isConnected()) {
            await browser.action.setPopup({ popup: 'src/popup/error.html' });
        } else if (!this.appEnabled) {
            await browser.action.setPopup({ popup: 'src/popup/disabled.html' });
        } else {
            await browser.action.setPopup({ popup: 'src/popup/popup.html' });
        }
    }

    // ─── Helpers ──────────────────────────────────────────────────────────────

    isMonitoringEnabled() {
        return this.appEnabled && !this.userDisabled && this.connector.isConnected();
    }

    _isSupportedProtocol(url) {
        if (!url) return false;
        try {
            return SUPPORTED_PROTOCOLS.includes(new URL(url).protocol);
        } catch {
            return false;
        }
    }

    // ─── Register all Firefox listeners ──────────────────────────────────────

    _register() {
        browser.downloads.onCreated.addListener(
            this._onDownloadCreated.bind(this)
        );
        browser.tabs.onUpdated.addListener(
            this._onTabUpdate.bind(this)
        );
        browser.tabs.onActivated.addListener(
            this._onTabActivated.bind(this)
        );
        browser.runtime.onMessage.addListener(
            this._onPopupMessage.bind(this)
        );
        browser.contextMenus.onClicked.addListener(
            this._onMenuClicked.bind(this)
        );

        this.requestWatcher.register();
        this._attachContextMenu();
    }
}
