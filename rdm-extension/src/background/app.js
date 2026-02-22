// src/background/app.js

import Connector     from './connector.js';
import RequestWatcher from './request-watcher.js';
import Logger        from './logger.js';

const SUPPORTED_PROTOCOLS = ['http:', 'https:', 'ftp:'];

export default class App {
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

    start() {
        this.connector.connect();
        this._register();
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
        // onDeterminingFilename is the preferred hook; onCreated is a fallback
        // for cases where the filename-determination phase doesn't fire.
        // Nothing extra needed here for now.
    }

    _onDeterminingFilename(download, suggest) {
        if (!this.isMonitoringEnabled()) return;

        const url = download.finalUrl || download.url;

        if (this._shouldTakeOver(url, download.filename)) {
            this.log.log('Intercepting download:', url);

            chrome.downloads.cancel(download.id, () => {
                chrome.downloads.erase({ id: download.id });
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
        chrome.cookies.getAll({ url }, cookies => {
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
        });
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

        // Return true to indicate we may respond asynchronously.
        return true;
    }

    // ─── Context menu ─────────────────────────────────────────────────────────

    _attachContextMenu() {
        chrome.contextMenus.create({
            id:       'rdm-download-link',
            title:    'Download with rdm',
            contexts: ['link', 'video', 'audio', 'all'],
        });
        chrome.contextMenus.create({
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
        const imageData = await this._getIconImageData(active);
        chrome.action.setIcon({ imageData });

        const count = this.videoList?.length ?? 0;
        chrome.action.setBadgeText({ text: count > 0 ? String(count) : '' });
        chrome.action.setBadgeBackgroundColor({ color: '#2196F3' });

        if (!this.connector.isConnected()) {
            chrome.action.setPopup({ popup: 'src/popup/error.html' });
        } else if (!this.appEnabled) {
            chrome.action.setPopup({ popup: 'src/popup/disabled.html' });
        } else {
            chrome.action.setPopup({ popup: 'src/popup/popup.html' });
        }
    }

    async _getIconImageData(active) {
        const sizes = [16, 48, 128];
        const suffix = active ? '' : '-mono';
        const imageData = {};
        await Promise.all(sizes.map(async (size) => {
            const url = chrome.runtime.getURL(`icons/icon${size}${suffix}.png`);
            const resp = await fetch(url);
            const blob = await resp.blob();
            const bitmap = await createImageBitmap(blob);
            const canvas = new OffscreenCanvas(size, size);
            const ctx = canvas.getContext('2d');
            ctx.drawImage(bitmap, 0, 0, size, size);
            imageData[size] = ctx.getImageData(0, 0, size, size);
        }));
        return imageData;
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

    // ─── Register all Chrome listeners ───────────────────────────────────────

    _register() {
        chrome.downloads.onCreated.addListener(
            this._onDownloadCreated.bind(this)
        );
        chrome.downloads.onDeterminingFilename.addListener(
            this._onDeterminingFilename.bind(this)
        );
        chrome.tabs.onUpdated.addListener(
            this._onTabUpdate.bind(this)
        );
        chrome.tabs.onActivated.addListener(
            this._onTabActivated.bind(this)
        );
        chrome.runtime.onMessage.addListener(
            this._onPopupMessage.bind(this)
        );
        chrome.contextMenus.onClicked.addListener(
            this._onMenuClicked.bind(this)
        );

        this.requestWatcher.register();
        this._attachContextMenu();
    }
}
