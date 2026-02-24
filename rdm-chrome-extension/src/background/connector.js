// src/background/connector.js

const RDM_BASE_URL       = "http://127.0.0.1:8597";
const ALARM_COUNT        = 12;       // 12 alarms staggered 5 s apart, each 1-min period
const ALARM_STAGGER_MS   = 5000;
const ALARM_INITIAL_DELAY_MS = 1000;

export default class Connector {
    /**
     * @param {(msg: object) => void} onMessage   Called whenever a valid sync payload arrives.
     * @param {(err: Error)  => void} onDisconnect Called when the rdm server cannot be reached.
     */
    constructor(onMessage, onDisconnect) {
        this._onMessage    = onMessage;
        this._onDisconnect = onDisconnect;
        this.connected     = false;
        this._boundOnAlarm = this._onAlarm.bind(this);
    }

    // ─── Public API ──────────────────────────────────────────────────────────

    /**
     * Kick off staggered alarms to keep the MV3 service worker alive and poll /sync.
     */
    connect() {
        for (let i = 0; i < ALARM_COUNT; i++) {
            chrome.alarms.create(`rdm-sync-${i}`, {
                periodInMinutes: 1,
                when: Date.now() + ALARM_INITIAL_DELAY_MS + (i + 1) * ALARM_STAGGER_MS,
            });
        }
        chrome.alarms.onAlarm.addListener(this._boundOnAlarm);

        // Do one immediate sync so we get the state as soon as the extension loads.
        this._sync();
    }

    /**
     * POST `data` as JSON to `RDM_BASE_URL + path`.
     * Returns the parsed JSON response (which always contains the sync payload).
     *
     * @param {string} path  e.g. "/download", "/media", "/vid", "/clear"
     * @param {object} data
     * @returns {Promise<object|null>}
     */
    async postMessage(path, data) {
        try {
            const res = await fetch(RDM_BASE_URL + path, {
                method:  "POST",
                headers: { "Content-Type": "application/json" },
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

    /** Whether the last poll/POST succeeded. */
    isConnected() {
        return this.connected;
    }

    /**
     * Attempt to wake rdm via a custom URI scheme.
     * Requires rdm to have registered the `rdm+app://` protocol with the OS.
     */
    launchApp() {
        // chrome.tabs.create({ url: "rdm+app://launch" });
        // Stub: left for future OS-level protocol registration in rdm.
    }

    // ─── Private ─────────────────────────────────────────────────────────────

    /** Fires on every chrome.alarms event; only reacts to our own alarms. */
    _onAlarm(alarm) {
        if (alarm.name.startsWith("rdm-sync-")) {
            this._sync();
        }
    }

    /** GET /sync heartbeat. */
    async _sync() {
        try {
            const res = await fetch(RDM_BASE_URL + "/sync");
            if (!res.ok) throw new Error(`HTTP ${res.status}`);
            const json = await res.json();
            this.connected = true;
            this._onResponse(json);
        } catch (err) {
            this._onError(err);
        }
    }

    _onResponse(payload) {
        this._onMessage(payload);
    }

    _onError(err) {
        this.connected = false;
        this._onDisconnect(err);
    }
}
