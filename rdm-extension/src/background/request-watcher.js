// src/background/request-watcher.js

export default class RequestWatcher {
    /**
     * @param {(data: object) => void} onRequest  Fired when a matching media request is detected.
     */
    constructor(onRequest) {
        this._onRequest = onRequest;

        // Runtime config — populated via updateConfig() from sync payload.
        this._mediaExts     = [];  // URL path extensions (mp4, m3u8, webm …)
        this._blockedHosts  = [];
        this._matchingHosts = [];  // always-capture hosts (googlevideo, etc.)
        this._mediaTypes    = [];  // Content-Type prefixes (audio/, video/)
        this._urlPatterns   = [];  // compiled RegExp objects

        // requestId → request info map, populated in onSendHeaders.
        this._pendingRequests = new Map();

        // Bind once so we can remove the same function reference later.
        this._boundOnSendHeaders    = this._onSendHeaders.bind(this);
        this._boundOnHeadersReceived = this._onHeadersReceived.bind(this);
        this._boundOnErrorOccurred  = this._onErrorOccurred.bind(this);
    }

    // ─── Public API ──────────────────────────────────────────────────────────

    /**
     * Update matching config from the rdm sync payload.
     * @param {{
     *   mediaExts:     string[],
     *   blockedHosts:  string[],
     *   matchingHosts: string[],
     *   mediaTypes:    string[],
     *   urlPatterns:   string[],
     * }} config
     */
    updateConfig(config) {
        this._mediaExts     = (config.mediaExts     ?? []).map(e => e.toUpperCase());
        this._blockedHosts  = config.blockedHosts  ?? [];
        this._matchingHosts = config.matchingHosts ?? [];
        this._mediaTypes    = config.mediaTypes    ?? [];

        // Compile regex patterns; skip any that are malformed.
        this._urlPatterns = [];
        for (const pattern of (config.urlPatterns ?? [])) {
            try {
                this._urlPatterns.push(new RegExp(pattern, 'i'));
            } catch (e) {
                console.warn('[rdm] Skipping malformed URL pattern:', pattern, e.message);
            }
        }
    }

    /** Start listening to webRequest events. */
    register() {
        chrome.webRequest.onSendHeaders.addListener(
            this._boundOnSendHeaders,
            { urls: ['http://*/*', 'https://*/*'] },
            ['extraHeaders', 'requestHeaders']
        );
        chrome.webRequest.onHeadersReceived.addListener(
            this._boundOnHeadersReceived,
            { urls: ['http://*/*', 'https://*/*'] },
            ['extraHeaders', 'responseHeaders']
        );
        chrome.webRequest.onErrorOccurred.addListener(
            this._boundOnErrorOccurred,
            { urls: ['http://*/*', 'https://*/*'] }
        );
    }

    /** Stop listening (called when monitoring is toggled off). */
    unregister() {
        chrome.webRequest.onSendHeaders.removeListener(this._boundOnSendHeaders);
        chrome.webRequest.onHeadersReceived.removeListener(this._boundOnHeadersReceived);
        chrome.webRequest.onErrorOccurred.removeListener(this._boundOnErrorOccurred);
    }

    // ─── Private — webRequest hooks ──────────────────────────────────────────

    _onSendHeaders(details) {
        this._pendingRequests.set(details.requestId, {
            requestId:      details.requestId,
            url:            details.url,
            method:         details.method,
            tabId:          details.tabId,
            requestHeaders: details.requestHeaders ?? [],
        });
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

        // Fetch tab info asynchronously so we can attach title + url.
        if (details.tabId >= 0) {
            chrome.tabs.get(details.tabId, tab => {
                if (chrome.runtime.lastError) {
                    // Tab may have been closed; fire without tab info.
                    this._onRequest(this._createRequestData(req, combined, '', '', details.tabId));
                    return;
                }
                this._onRequest(
                    this._createRequestData(req, combined, tab.title ?? '', tab.url ?? '', details.tabId)
                );
            });
        } else {
            this._onRequest(this._createRequestData(req, combined, '', '', details.tabId));
        }
    }

    _onErrorOccurred(details) {
        this._pendingRequests.delete(details.requestId);
    }

    // ─── Private — matching logic ─────────────────────────────────────────────

    /**
     * Returns true if the request+response pair should be sent to rdm.
     * Rules evaluated in order (first match wins; blocked host always skips).
     */
    _isMatchingRequest({ req, url, responseHeaders }) {
        let hostname;
        try {
            hostname = new URL(url).hostname.toLowerCase();
        } catch {
            return false;
        }

        // 1. Blocked host → skip always
        if (this._blockedHosts.some(h => hostname.includes(h.toLowerCase()))) {
            return false;
        }

        const pathUpper = new URL(url).pathname.toUpperCase();
        const contentType        = this._getHeader(responseHeaders, 'content-type') ?? '';
        const contentDisposition = this._getHeader(responseHeaders, 'content-disposition') ?? '';

        // 2. URL path ends with a media extension (mp4, m3u8, webm …)
        if (this._mediaExts.some(ext => pathUpper.endsWith('.' + ext) || pathUpper.endsWith(ext))) {
            return true;
        }

        // 3. URL matches a URL pattern regex
        if (this._urlPatterns.some(re => re.test(url))) {
            return true;
        }

        // 4. Content-Type starts with a tracked media-type prefix (audio/, video/)
        if (this._mediaTypes.some(mt => contentType.toLowerCase().startsWith(mt.toLowerCase()))) {
            return true;
        }

        // 5. Content-Disposition reveals a matching file extension
        if (contentDisposition) {
            const cdUpper = contentDisposition.toUpperCase();
            if (this._mediaExts.some(ext => cdUpper.includes('.' + ext))) {
                return true;
            }
        }

        // 6. Always-capture host (e.g. googlevideo.com)
        if (this._matchingHosts.some(h => hostname.includes(h.toLowerCase()))) {
            return true;
        }

        return false;
    }

    // ─── Private — payload builder ────────────────────────────────────────────

    /**
     * Builds the object POSTed to /media.
     */
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

    // ─── Utilities ────────────────────────────────────────────────────────────

    /** Convert a webRequest header array to a plain { name: [value, ...] } dict. */
    _headersToDict(headers) {
        const dict = {};
        for (const h of (headers ?? [])) {
            const key = h.name;
            if (!dict[key]) dict[key] = [];
            dict[key].push(h.value ?? '');
        }
        return dict;
    }

    /** Case-insensitive header lookup in a webRequest header array. */
    _getHeader(headers, name) {
        const lower = name.toLowerCase();
        const found = (headers ?? []).find(h => h.name.toLowerCase() === lower);
        return found?.value ?? null;
    }
}
