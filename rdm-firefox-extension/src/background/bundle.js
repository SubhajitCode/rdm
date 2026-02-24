// src/background/bundle.js
// Firefox MV2 does not support ES module background scripts.
// This file concatenates logger, connector, request-watcher, and app in order
// so they are all available in the same global scope, then starts the App.
//
// To rebuild this file run:  npm run build  (or concatenate the four source
// files manually in the order below).
//
// ─────────────────────────────────────────────────────────────────────────────
// 1. Logger
// ─────────────────────────────────────────────────────────────────────────────

class Logger {
    constructor() {
        this.enabled = true;
    }

    log(...args) {
        if (this.enabled) console.log('[rdm]', ...args);
    }

    warn(...args) {
        if (this.enabled) console.warn('[rdm]', ...args);
    }

    error(...args) {
        console.error('[rdm]', ...args);
    }

    setEnabled(value) {
        this.enabled = !!value;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. Connector
// ─────────────────────────────────────────────────────────────────────────────

const RDM_BASE_URL           = "http://127.0.0.1:8597";
const ALARM_COUNT            = 12;
const ALARM_STAGGER_MS       = 5000;
const ALARM_INITIAL_DELAY_MS = 1000;

class Connector {
    constructor(onMessage, onDisconnect) {
        this._onMessage    = onMessage;
        this._onDisconnect = onDisconnect;
        this.connected     = false;
        this._boundOnAlarm = this._onAlarm.bind(this);
    }

    async connect() {
        for (let i = 0; i < ALARM_COUNT; i++) {
            browser.alarms.create(`rdm-sync-${i}`, {
                periodInMinutes: 1,
                when: Date.now() + ALARM_INITIAL_DELAY_MS + (i + 1) * ALARM_STAGGER_MS,
            });
        }
        browser.alarms.onAlarm.addListener(this._boundOnAlarm);
        // Await the first sync so callers can wait for config to be populated.
        await this._sync();
    }

    async postMessage(path, data) {
        try {
            const res = await fetch(RDM_BASE_URL + path, {
                method:  'POST',
                headers: { 'Content-Type': 'application/json' },
                body:    JSON.stringify(data),
            });
            if (!res.ok) throw new Error(`HTTP ${res.status}`);
            const json = await res.json();
            this.connected = true;
            this._onResponse(json);
            return json;
        } catch (err) {
            this._onError(err);
            return null;
        }
    }

    isConnected() { return this.connected; }

    launchApp() {
        // Stub: OS-level protocol launch not yet wired up.
    }

    _onAlarm(alarm) {
        if (alarm.name.startsWith('rdm-sync-')) this._sync();
    }

    async _sync() {
        try {
            const res = await fetch(RDM_BASE_URL + '/sync');
            if (!res.ok) throw new Error(`HTTP ${res.status}`);
            const json = await res.json();
            this.connected = true;
            this._onResponse(json);
        } catch (err) {
            this._onError(err);
        }
    }

    _onResponse(payload) { this._onMessage(payload); }
    _onError(err)        { this.connected = false; this._onDisconnect(err); }
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. RequestWatcher
// ─────────────────────────────────────────────────────────────────────────────

class RequestWatcher {
    constructor(onRequest) {
        this._onRequest      = onRequest;
        this._mediaExts      = [];
        this._blockedHosts   = [];
        this._matchingHosts  = [];
        this._mediaTypes     = [];
        this._urlPatterns    = [];
        this._pendingRequests = new Map();

        this._boundOnSendHeaders     = this._onSendHeaders.bind(this);
        this._boundOnHeadersReceived = this._onHeadersReceived.bind(this);
        this._boundOnErrorOccurred   = this._onErrorOccurred.bind(this);
    }

    updateConfig(config) {
        this._mediaExts     = (config.mediaExts     ?? []).map(e => e.toUpperCase());
        this._blockedHosts  = config.blockedHosts  ?? [];
        this._matchingHosts = config.matchingHosts ?? [];
        this._mediaTypes    = config.mediaTypes    ?? [];

        this._urlPatterns = [];
        for (const pattern of (config.urlPatterns ?? [])) {
            try {
                this._urlPatterns.push(new RegExp(pattern, 'i'));
            } catch (e) {
                console.warn('[rdm] Skipping malformed URL pattern:', pattern, e.message);
            }
        }
    }

    register() {
        browser.webRequest.onSendHeaders.addListener(
            this._boundOnSendHeaders,
            { urls: ['http://*/*', 'https://*/*'] },
            ['requestHeaders']
        );
        // 'blocking' is required in Firefox MV2 for responseHeaders to be
        // delivered in the callback. Without it the array is always empty,
        // breaking Content-Type and Content-Disposition matching.
        browser.webRequest.onHeadersReceived.addListener(
            this._boundOnHeadersReceived,
            { urls: ['http://*/*', 'https://*/*'] },
            ['responseHeaders', 'blocking']
        );
        browser.webRequest.onErrorOccurred.addListener(
            this._boundOnErrorOccurred,
            { urls: ['http://*/*', 'https://*/*'] }
        );
    }

    unregister() {
        browser.webRequest.onSendHeaders.removeListener(this._boundOnSendHeaders);
        browser.webRequest.onHeadersReceived.removeListener(this._boundOnHeadersReceived);
        browser.webRequest.onErrorOccurred.removeListener(this._boundOnErrorOccurred);
    }

    _onSendHeaders(details) {
        // Cap map size to prevent unbounded memory growth from requests
        // that never complete (no onHeadersReceived or onErrorOccurred).
        if (this._pendingRequests.size >= 2000) {
            const firstKey = this._pendingRequests.keys().next().value;
            this._pendingRequests.delete(firstKey);
        }

        const req = {
            requestId:      details.requestId,
            url:            details.url,
            method:         details.method,
            tabId:          details.tabId,
            requestHeaders: details.requestHeaders ?? [],
        };
        this._pendingRequests.set(details.requestId, req);

        // Fast-path: fire immediately for always-capture hosts (e.g. googlevideo.com)
        // without waiting for onHeadersReceived. This avoids missing requests that
        // are redirected or blocked before response headers arrive.
        if (this._isMatchingByUrl(details.url)) {
            this._pendingRequests.delete(details.requestId);
            const combined = { req, url: details.url, responseHeaders: [], tabId: details.tabId };
            this._dispatchRequest(req, combined, details.tabId);
        }
    }

    _onHeadersReceived(details) {
        const req = this._pendingRequests.get(details.requestId);
        if (!req) return;
        this._pendingRequests.delete(details.requestId);

        const combined = {
            req,
            url:             details.url,
            responseHeaders: details.responseHeaders ?? [],
            tabId:           details.tabId,
        };

        if (!this._isMatchingRequest(combined)) return;
        this._dispatchRequest(req, combined, details.tabId);
    }

    _dispatchRequest(req, combined, tabId) {
        if (tabId >= 0) {
            browser.tabs.get(tabId).then(tab => {
                this._onRequest(
                    this._createRequestData(req, combined, tab.title ?? '', tab.url ?? '', tabId)
                );
            }).catch(() => {
                this._onRequest(this._createRequestData(req, combined, '', '', tabId));
            });
        } else {
            this._onRequest(this._createRequestData(req, combined, '', '', tabId));
        }
    }

    _onErrorOccurred(details) {
        this._pendingRequests.delete(details.requestId);
    }

    _isMatchingRequest({ req, url, responseHeaders }) {
        let hostname;
        try {
            hostname = new URL(url).hostname.toLowerCase();
        } catch {
            return false;
        }

        if (this._blockedHosts.some(h => hostname.includes(h.toLowerCase()))) return false;

        const pathUpper          = new URL(url).pathname.toUpperCase();
        const contentType        = this._getHeader(responseHeaders, 'content-type') ?? '';
        const contentDisposition = this._getHeader(responseHeaders, 'content-disposition') ?? '';

        if (this._mediaExts.some(ext => pathUpper.endsWith('.' + ext) || pathUpper.endsWith(ext))) return true;
        if (this._urlPatterns.some(re => re.test(url))) return true;
        if (this._mediaTypes.some(mt => contentType.toLowerCase().startsWith(mt.toLowerCase()))) return true;

        if (contentDisposition) {
            const cdUpper = contentDisposition.toUpperCase();
            if (this._mediaExts.some(ext => cdUpper.includes('.' + ext))) return true;
        }

        if (this._matchingHosts.some(h => hostname.includes(h.toLowerCase()))) return true;

        return false;
    }

    // URL-only match: used in the fast-path from onSendHeaders for hosts that
    // should always be captured regardless of response headers (e.g. googlevideo.com).
    _isMatchingByUrl(url) {
        let hostname;
        try {
            hostname = new URL(url).hostname.toLowerCase();
        } catch {
            return false;
        }

        if (this._blockedHosts.some(h => hostname.includes(h.toLowerCase()))) return false;

        const pathUpper = new URL(url).pathname.toUpperCase();
        if (this._mediaExts.some(ext => pathUpper.endsWith('.' + ext) || pathUpper.endsWith(ext))) return true;
        if (this._urlPatterns.some(re => re.test(url))) return true;
        if (this._matchingHosts.some(h => hostname.includes(h.toLowerCase()))) return true;

        return false;
    }

    _createRequestData(req, combined, tabTitle, tabUrl, tabId) {
        const reqHeaders  = this._headersToDict(req.requestHeaders);
        const respHeaders = this._headersToDict(combined.responseHeaders);

        const cookieHeader = reqHeaders['Cookie'] ?? reqHeaders['cookie'] ?? '';
        const cookieStr    = Array.isArray(cookieHeader) ? cookieHeader.join('; ') : cookieHeader;

        return {
            url:             combined.url,
            file:            tabTitle,
            requestHeaders:  reqHeaders,
            responseHeaders: respHeaders,
            cookie:          cookieStr,
            method:          req.method,
            userAgent:       navigator.userAgent,
            tabUrl:          tabUrl,
            tabId:           String(tabId),
        };
    }

    _headersToDict(headers) {
        const dict = {};
        for (const h of (headers ?? [])) {
            const key = h.name;
            if (!dict[key]) dict[key] = [];
            dict[key].push(h.value ?? '');
        }
        return dict;
    }

    _getHeader(headers, name) {
        const lower = name.toLowerCase();
        const found = (headers ?? []).find(h => h.name.toLowerCase() === lower);
        return found?.value ?? null;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. App
// ─────────────────────────────────────────────────────────────────────────────

const SUPPORTED_PROTOCOLS = ['http:', 'https:', 'ftp:'];

class App {
    constructor() {
        this.log = new Logger();

        this.videoList    = [];
        this.blockedHosts = [];
        this.fileExts     = [];
        this.tabsWatcher  = [];
        this.userDisabled = false;
        this.appEnabled   = false;
        this.activeTabId  = -1;

        this.connector = new Connector(
            this.onMessage.bind(this),
            this.onDisconnect.bind(this)
        );
        this.requestWatcher = new RequestWatcher(
            this.onRequestDataReceived.bind(this)
        );
    }

    async start() {
        console.log("[rdm] Starting app...");
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

    onRequestDataReceived(data) {
        if (this.isMonitoringEnabled()) {
            this.connector.postMessage('/media', data);
        }
    }

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

    _updateActionIcon() {
        this._updateActionIconAsync().catch(err => this.log.warn('setIcon failed:', err));
    }

    async _updateActionIconAsync() {
        const active = this.isMonitoringEnabled();
        const suffix = active ? '' : '-mono';

        await browser.action.setIcon({
            path: {
                16:  `icons/icon16${suffix}.png`,
                48:  `icons/icon48${suffix}.png`,
                128: `icons/icon128${suffix}.png`,
            },
        });

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

    _register() {
        browser.downloads.onCreated.addListener(this._onDownloadCreated.bind(this));
        browser.tabs.onUpdated.addListener(this._onTabUpdate.bind(this));
        browser.tabs.onActivated.addListener(this._onTabActivated.bind(this));
        browser.runtime.onMessage.addListener(this._onPopupMessage.bind(this));
        browser.contextMenus.onClicked.addListener(this._onMenuClicked.bind(this));
        this.requestWatcher.register();
        this._attachContextMenu();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. Entry point
// ─────────────────────────────────────────────────────────────────────────────

console.log("[rdm] Background script loaded");
(async () => {
    const app = new App();
    await app.start();
})();
